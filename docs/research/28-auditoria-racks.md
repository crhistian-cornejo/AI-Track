# 28 · Auditoría de racks frente a Ableton Live

Fecha: 2026-11-21. Auditoría de solo lectura (agente Explore sobre el código de `main`, commit `21a6805`); no se ejecutó la app. Origen: el dueño dice que "los racks no se pueden guardar" y que "no se hacen racks como lo hace Ableton".

**Prefijos de ruta** (bajo `crates/`): **M** = `model/src`, **A** = `app/src`, **G** = `engine/src`, **K** = `stock/src`.

**Estados:** ✅ hecho · 🟡 parcial · ❌ falta.

## Resumen

- Confirmado: **nada guarda un rack como preset**. `user_presets` solo guarda parámetros de dispositivos stock.
- Un rack nuevo **nace inerte**: sin cadenas, sin zona "Drop here" y el motor lo deja en silencio.
- **Group, Ungroup y Fold borran mapeos de macro** y de moduladores (se implementan como Remove + Add, no como `MoveDevice`). Es pérdida de datos.
- Las cadenas y los pads **no tienen edición** (añadir, renombrar, color, reordenar, ocultar la lista; soltar samples en pads).

## A. Crear

| Punto | Estado | Hoy y evidencia |
|---|---|---|
| 1 Rack vacío | 🟡 | Listado en Cmd+K › Devices y Add Device (A/real/plugins.rs:66-71). Nace sin cadenas (A/state.rs:1829-1839), no pinta "Drop here" ni "+ Device" (A/views/devices/rack_ui.rs:373-418,462) y el motor lo deja en silencio (G/graph.rs:724,766-805): inerte. |
| 2 Cmd+G | 🟡 | Selección: Cmd/Ctrl+clic o Shift+clic en el título, dentro de una cadena (A/editing/racks.rs:40-88). Tipo: Instrument si hay instrumento, MIDI si todos son `midi.*`, si no Audio (M/racks.rs:110-117; un MIDI Rack anidado cuenta como Audio). Un paso de undo (A/editing/racks.rs:92-106). Mismos ids: el motor conserva el plugin vivo (G/shared.rs:224-329) y la automatización. **Atención:** es Remove + Add (M/racks.rs:129) y borra mapeos de macro del rack padre y de moduladores (M/command.rs:745,1142-1243). Los no adyacentes reordenan la señal. |
| 3 Ungroup | 🟡 | Solo ≤ 1 cadena y no Drum (M/racks.rs:136). Con varias avisa sin actuar (A/editing.rs:566). Varios racks, un paso (A/editing/racks.rs:117). Mismo borrado de mapeos. |
| 4 Menú contextual | ✅ | Group/Ungroup, Fold, Delete (A/editing/menus.rs:501-570); sin Duplicate, Rename ni Save. |
| 5 Anidados | ✅ | Recursivo (M/project.rs:245; G/compile.rs:296). El modo Map ignora `returns` (rack_ui.rs:162 frente a M/command.rs:904). |
| 6 Drum Rack desde samples | ❌ | Los pads no tienen `on_drop` (rack_ui.rs:625-741); nacen solo con Slice (M/slice.rs:183), Bloom o la IA. Único "mover": Receive ±1 (rack_ui.rs:499). Choke, sends y returns sí están (rack_ui.rs:496,793). |
| 7 Drop y mover | 🟡 | Mover entre cadenas funciona: arrastrar el título (A/views/devices.rs:441) a huecos (:156), fila de cadena (rack_ui.rs:594) o "Drop here" (:392); un paso (:248). **Crear una cadena arrastrando: falta.** |

## B. Cadenas

| Punto | Estado | Hoy y evidencia |
|---|---|---|
| Mostrar u ocultar la lista | ❌ | Siempre visible (rack_ui.rs:353); macros (devices.rs:471) y zonas (rack_ui.rs:533) sí se ocultan. |
| Añadir, renombrar, color, reordenar | ❌ | `AddChain` y `ChainProp::Name/Color` solo en el modelo (M/command.rs:81,182); no hay mover cadena; las filas no tienen arrastre ni doble clic (rack_ui.rs:576-618). Solo Cmd+G crea la primera cadena. |
| Borrar | ✅ | rack_ui.rs:540. |
| Activador, solo, mute | ✅ | Chips M/S (rack_ui.rs:484,608); el activador es el mute invertido. |
| Volumen, pan, sends | 🟡 | Volumen y pan salvo en MIDI Rack (rack_ui.rs:475; G/compile.rs:209); sends solo en Drum con returns (M/command.rs:850); `ChainOut::Return` sin UI. |
| Zonas y fades | ✅ | rack_ui.rs:865-1005; G/graph.rs:674,733. El selector y el volumen de cadena no son automatizables (M/project.rs:1327). |
| Hot-Swap | ❌ | Solo Alt+↑/↓ en stock (A/workspace.rs:3382). |
| Plegar | 🟡 | El dispositivo sí (devices.rs:383), pero como Remove + Add (:564): pierde mapeos. La lista de cadenas no se pliega. |

## C. Macros

| Punto | Estado | Hoy y evidencia |
|---|---|---|
| 8 a 16 | 🟡 | El modelo tiene 16 (M/project.rs:25); el botón 8/16 es solo de vista, sin guardar (rack_ui.rs:49,1076). |
| Map | ✅ | rack_ui.rs:1084,1209; A/workspace.rs:1962. |
| Rango, invertir, lista | ✅ | rack_ui.rs:1010,1155. |
| Renombrar | ❌ | `SetMacroDef` existe sin UI. |
| Randomize | ✅ | Rand y Lock (rack_ui.rs:1088). |
| Variaciones | 🟡 | Nueva, lanzar, borrar (rack_ui.rs:1094; A/state.rs:1771); faltan Overwrite y Rename. |
| Automatizar | ✅ | A/workspace.rs:1528; G/graph.rs:977. |
| Mapear a MIDI | ❌ | Nada. |

## D. Guardar y cargar

| Punto | Estado | Hoy y evidencia |
|---|---|---|
| Racks dentro del proyecto | ✅ | serde (M/project.rs:1000; test :1472); el estado de los plugins se captura al guardar (A/project_io.rs:465). |
| Rack como preset | ❌ | `save_user` solo guarda parámetros (A/views/device_window/presets.rs:113); la cabecera exige `DeviceKind::Stock` (…/meters.rs:48). |
| Verlo y añadirlo | ❌ | La pestaña Presets muestra solo los de fábrica de stock, con caché única (A/views/palette/mod.rs:414,568); los de usuario solo salen en la cabecera del dispositivo (chrome.rs:61). Añadir un preset cuesta 2 pasos de undo (:1101). |
| Cadena sola, predeterminado, Hot-Swap, racks de fábrica | ❌ | Mid-Side y Multiband existen solo como dispositivos; `K/../presets` no tiene `rack.*`. |
| Proyectos antiguos | ✅ | `format_version = 1`, `MIGRATIONS` vacío (M/format.rs:46); los campos nuevos llevan `serde(default)` (test M/command.rs:2466); **`Chain` y `Rack` tienen campos obligatorios sin default.** |

## E. Infraestructura reutilizable

- **Presets stock:** JSON `{name, plugin, author, tags, params{id: 0..1}}` (K/presets.rs:20). Los de usuario van en `<config>/user_presets/<plugin>/<Nombre>.json` (A/views/device_window/chrome.rs:53); config en `~/Library/Application Support/AI-Track`, `%APPDATA%\AI-Track` o `AITRACK_CONFIG_DIR` (A/appearance.rs:135). Nombre ≤ 48 caracteres sin `/\:*?"<>|`; repetir el nombre sobrescribe; sin `state`. Los favoritos son solo de dispositivos (A/plugin_prefs.rs:8-22). El Content Hub son packs e instalador (A/views/hub.rs:15).
- **Reutilizable:** popover de nombre (chrome.rs:212,464); `list_user`/`merge` (…/presets.rs:96; filtra `.json`, :101); `write_atomic` (A/project_io.rs:99); `copy_devices` + `relink_devices` (M/tracks.rs:147-187: ids nuevos y mapeos reenlazados); `sampler_state` (M/slice.rs:176). `PresetRow` usa `&'static str` (A/views/palette/mod.rs:149): hay que pasarlo a `String`.
- **Serde:** `Device { id, name, kind (tag "type": stock | plugin | rack), enabled, params, state (base64), preset, collapsed, sidechain, mods }` (M/project.rs:1000,1367). El blob de VST/CLAP solo está al día tras `capture_plugin_states` (A/state.rs:2254) y solo en proceso (A/real/plugins.rs:240); con sandbox (desactivado por defecto) no se captura.
- **IDs:** contador único (M/ids.rs:24); `AddDevice` valida solo el id raíz (M/command.rs:735); `observe_devices` sube el contador (:1105).
- **Ausentes:** un plugin ausente pasa a `Missing`, se salta con aviso y conserva su `state` (G/compile.rs:387). Un sample es una ruta absoluta dentro del state; si falta, el sampler queda vacío sin aviso (K/instrument/sampler.rs:781); `FindMissingFiles` cubre solo `Source::Audio` (M/paths.rs:96).

### Formato propuesto: `.aitrack-rack`

JSON UTF-8 en `<config>/user_racks/<kind>/<Nombre>.aitrack-rack`; los de fábrica se embeben en la compilación como hace K/presets.rs:17.

```json
{
  "format": "aitrack-rack",
  "format_version": 1,
  "app_version": "0.0.8",
  "name": "…", "author": "…", "tags": [],
  "kind": "audio | instrument | midi | drum",
  "payload": { "device": { } },
  "requires": {
    "plugins": [{ "format": "vst3|clap", "uid": "…", "name": "…", "vendor": "…" }],
    "samples": [{ "hint": "…", "hash": "…", "size": 0 }]
  }
}
```

`payload` es un `Device` o, para guardar una cadena sola, un `Chain`; `sidechain` va a `null`. Versión propia con `migrate_with` (M/format.rs:67).

### Flujo propuesto

- **Guardar:** menú del rack "Save as Preset…" → `capture_plugin_states` → clonar y limpiar → `write_atomic` → invalidar la caché de la paleta.
- **Cargar:** Enter en Cmd+K, arrastre (patrón `DraggedSample`, A/workspace.rs:471) o `ExternalPaths` (A/views/arrangement.rs:696) → comprobar versión → `instantiate` (ids frescos y relink) → avisar de lo ausente → un solo `AddDevice`.

## F. Riesgos

1. **IDs.** `AddChain`, `UpdateChain` y `AddDevice` no detectan duplicados anidados (M/command.rs:735,809,826) y el pool comparte procesador por `DeviceId` (G/shared.rs:307): hay que remapear todo antes de construir el comando.
2. **Undo.** Un único `AddDevice` o `Batch`. Group, Ungroup y Fold deben usar `MoveDevice`, que conserva los mapeos (test M/command.rs:2245).
3. **Motor.** Toda edición estructural recompila el grafo, y también los arrastres de volumen o de zonas de cadena (G/compile.rs:1164-1229; `SetChainProp` no es `FastEdit`, A/state.rs:100-134). Cargar N VST3 bloquea hasta 30 s (A/real/plugins.rs:34,339).
4. **PDC.** La latencia del rack es el máximo de sus cadenas más los returns, y se compensa (G/compile.rs:296-348); las cadenas nuevas entran desde silencio (G/graph.rs:941).
5. **Compatibilidad.** Los campos nuevos llevan `#[serde(default)]`; el preset lleva versión propia y rechaza las más nuevas (patrón `TooNew`, M/format.rs:67-80).

## Prioridad sugerida por la auditoría

1. `MoveDevice` en Group, Ungroup y Fold (hoy pierden datos).
2. UI de cadenas (añadir, renombrar, color, reordenar, ocultar la lista) y zona que crea cadena.
3. Guardar y cargar racks.
4. Samples en pads.
5. Macros: renombrar, Overwrite/Rename, mapeo MIDI.

> Nota del líder: el dueño pidió explícitamente guardar racks, así que el punto 3 sube al primer bloque de entrega junto al 1.
