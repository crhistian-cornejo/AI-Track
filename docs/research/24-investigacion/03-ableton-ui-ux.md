# 24-03 · Interfaz y flujo de Ableton Live 12.4 / 13 que aún nos faltan

Fecha: 17-nov-2026. Trabajo de investigación: no se tocó código de producción, no hay commits.

**Qué es este informe.** Una lectura de la interfaz de Ableton Live en su estado más reciente (Live 12.4.16 estable y Live 13 en beta pública), contrastada con lo que hoy tiene `crates/app`, para decidir qué patrones de interfaz y de flujo nos faltan y en qué orden construirlos. Construye encima de [08](../08-ableton-live-manual.md) (estructura del manual y control bar), [13](../13-auditoria-paridad-ableton.md) (paridad funcional), [20](../20-hoja-de-ruta.md) (olas), [21](../21-referencias-ui-dispositivos.md) y [23](../23-nivel-de-acabado.md) (aspecto y acabado de los dispositivos). No repite sus tablas: cuando algo de esos informes ya está resuelto en el código o ha cambiado, se dice en §1.

**Regla de producto.** Replicamos función y flujo, nunca la marca ni la apariencia exacta: colores de `theme.rs`, tiempos de `motion.rs`, iconos de `icons.rs`, espaciado múltiplo de 4 px (CLAUDE.md).

## 0. Método y convenciones

- **[V]** = verificado: lo leí en la fuente (página oficial descargada o código con `ruta:línea`). **[S]** = supuesto o inferido sin comprobación directa.
- Rutas de código relativas a `crates/`. Leí `app/src/state.rs`, `keys.rs`, `commands.rs`, `workspace.rs`, `theme.rs`, `motion.rs` y las vistas de `app/src/views/` que se citan, y miré las 8 capturas de `docs/screenshots/` (arrangement, device view, paleta Cmd+K y lanes de automatización).
- Fuentes de Ableton: notas de versión de [Live 12](https://www.ableton.com/en/release-notes/live-12/) (última entrada: 12.4.16, 30-oct-2026) y de la [beta de Live 13](https://www.ableton.com/en/release-notes/live-13-beta/) (13.0b25, 10-nov-2026), el [anuncio de Live 13](https://www.ableton.com/en/blog/live-13-is-coming/) (10-nov-2026, salida "early 2027") y el [manual de Live 12](https://www.ableton.com/en/live-manual/12/). Todo [V] salvo marca contraria.
- **Capturas.** El navegador interactivo de la plataforma solo corre en máquinas en la nube y no había créditos para una; las páginas se descargaron con Chrome sin ventana (`--headless --dump-dom`) en este equipo y las 15 capturas son **las figuras originales de esas páginas**, bajadas a resolución completa y convertidas a PNG. Cada pie dice de qué página sale. Las imágenes son © de Ableton, Bitwig y Apple; se guardan solo como referencia interna de investigación y no deben ir al producto ni a material público.
- Esfuerzo: **S** = días · **M** = 1-2 semanas · **L** = más.

### Resumen

Desde el informe 13 (15-nov) el código avanzó mucho: Session editable, gestión de pistas, routing de E/S, grabación, autosave y recuperación, freeze, take lanes, panel de Sonidos con preview a tempo y similitud tímbrica ya existen (§1). Lo que más bloquea ahora hacer una canción **no es una función de motor sino de flujo de interfaz**: no hay **locators** para estructurar la canción, el editor MIDI no ve **varios clips a la vez**, las **MIDI Tools** son acciones ciegas sin parámetros ni previsualización, la automatización no tiene **formas, asas de escalado ni simplificación**, no existe **Bounce** (solo freeze/flatten), no se puede **apilar Clip y Devices** en el panel inferior y no hay ninguna **ayuda contextual** (Info View) ni navegación por foco de teclado. Live 13 añade tres patrones que vale la pena copiar pronto porque son baratos y aceleran todo: **Capture Automation**, **navegador contextual en la cadena de dispositivos** y **device slots en el mixer**.

---

## 1. Qué ha cambiado desde los informes 13 y 20

| Hueco que señalaban 13/20 | Estado hoy | Evidencia |
|---|---|---|
| Session View sin crear/mover clips ni escenas (13 §1.3, crítico) | **Resuelto** | `app/src/session_edit.rs:173` crear clip MIDI en slot, `:214` mover, `:236` copiar, `:303-355` añadir/borrar/duplicar/renombrar escena, `:417` soltar archivos; `views/session.rs:91-207` botones de escena [V] |
| Borrar pista (13 §1.6, crítico) | **Resuelto** | `app/src/keys.rs:223` "Delete Track" (`mod-backspace`) [V] |
| Selector de salida/entrada/monitor del mixer (13 §2.2) | **Resuelto** | `app/src/views/mixer.rs:261-287`, `app/src/mixer_routing.rs:59-145` [V] |
| Seg. BPM editable con ÷2/×2 (20 0.16) | **Resuelto** | `app/src/views/bottom.rs:326-362` (`SegBpm::Half/Double/Step/Set`) [V] |
| Count-in, cuantización global, Back to Arrangement en la barra (20 1.15) | **Resuelto** | `app/src/views/topbar.rs:485, 494, 502` [V] |
| Entrada MIDI de hardware (13 §1.2, crítico) | **Resuelto** (abre puertos) | `app/src/real/engine.rs:202` `open_midi_input` [V] |
| Botón Record que mentía (13 §2.1) | **Resuelto** | `app/src/views/topbar.rs:189-199` `record_press`/`can_record` y test `record_never_arms_silently` (`:719`) [V] |
| Autosave, recuperación y reparar archivos (13 §1.9) | **Resuelto** | `app/src/autosave.rs:20-49`; `app/src/workspace.rs:3375-3377` `offer_recovery`/`offer_relink`; acción "Collect All and Save" en `keys.rs:276` [V] |
| Etiqueta de undo invisible (13 §2.17) | **Parcial** | la barra de estado muestra "Undo: …" (`workspace.rs:2747`); sigue sin panel de historial ni menú Edit nativo (no hay `set_menus` en `main.rs`) [V] |
| Freeze/Flatten (13 §1.4) | **Resuelto**; **Bounce sigue ausente** | `app/src/freeze.rs:116-153`; `grep -i bounce crates/app/src` vacío [V] |
| Take lanes fantasma (13 §2.7) | **Resuelto** | `keys.rs:225` "Show Take Lanes", `app/src/take_lanes.rs` [V] |
| Preview no sincronizado (13 §2.14, 20 3.2-3.3) | **Resuelto** con el panel de Sonidos | `app/src/views/sounds/mod.rs:1-9` ("hear them already adjusted to the song (stretched to its tempo, moved to its key)") y lista virtualizada [V] |
| Similitud tímbrica (13 §1.8) | **Resuelto** en la paleta | `app/src/views/palette/mod.rs:63, 205` (`SimilarTo`) [V] |
| Arrastrar archivos del sistema (13 §1.8) | **Resuelto** | `views/arrangement.rs:618-619` `ExternalPaths` [V] |
| Rejilla adaptativa (PLAN §7.1 P0-2) | **Resuelto** | `app/src/state.rs:213-246` (`Snap::Adaptive`) [V] |
| Follow actions sin UI (13 §1.3) | **Sigue ausente** | el modelo tiene `follow: Option<FollowAction>` (`model/src/project.rs:674`) y `grep follow_action crates/app/src` está vacío [V] |
| Locators / marcadores (13 §1.4) | **Sigue ausente** | ni el modelo ni la app tienen locators (`grep -i locator crates/model/src` vacío) [V] |
| Probabilidad por nota sin UI (13 §2.8) | **Sigue ausente** | `probability` solo colorea la nota (`views/piano_roll.rs:239`); el único carril es el de velocity (`:265`) [V] |
| MIDI Tools con parámetros (13 §1.5) | **Sigue parcial** | menú de acciones de un clic (`views/menus.rs:352-460`: acordes, inversión, progresión, arpegio, strum, chop, randomize) [V] |

---

## 2. Novedades de Live 12.4 y Live 13 que el informe 13 no cubría

El informe 13 cerró con Live 13 sacado de prensa. Ahora están las notas oficiales de la beta [V], y algunas cosas son distintas de lo que decía la prensa (por ejemplo, el Step Sequencer es un **tipo de clip**, no un dispositivo).

| Versión | Novedad (oficial) | Patrón de interfaz que enseña | AI-Track hoy |
|---|---|---|---|
| 12.4 | **Learn View** sustituye a la Help View: módulos con vídeo y texto, progreso marcable, vídeo en ventana flotante ([help.ableton.com](https://help.ableton.com/hc/en-us/articles/26963870210076-What-s-new-in-Live-12-4)) | ayuda dentro de la app, al lado del trabajo | nada; solo la hoja de atajos (`keys.rs:270`) |
| 12.4 | **Stem Separation** de solo la parte audible o de una selección de tiempo; **Merge to single track** (misma fuente) | la acción respeta la selección y produce menos pistas | stems vía chat (13 §1.8); sin selección parcial ni "fusionar" [S sobre el detalle actual] |
| 12.4 | **Copy Time** (`Cmd+Shift+C`), zona de redimensionado de notas cortas ampliada, `S` hace solo sobre la cadena seleccionada (misma fuente) | pequeñas fricciones quitadas | no hay Copy Time en `keys.rs` [V] |
| 12.4.15 | `Cmd+F` selecciona el término existente en vez de borrarlo ([notas 12](https://www.ableton.com/en/release-notes/live-12/)) | buscar de nuevo sin reescribir | a comprobar en la paleta [S] |
| 13 | **Capture Automation** (botón Capture o `N`): recupera los movimientos de mando que hiciste sin grabar, en cualquier pista, varios parámetros a la vez ([notas 13](https://www.ableton.com/en/release-notes/live-13-beta/)) | "no hace falta armar nada" | sin Capture MIDI ni Capture Automation (13 §1.2) [V] |
| 13 | **Contextual Browsing**: navegador que se abre desde el "+" de la cadena o con doble clic entre dispositivos, filtrado por tipo de pista y posición, con Recientes y presets del dispositivo (`Enter`), navegación completa por teclado | insertar sin salir del sitio | menú "Add device" con buscador (`views/menus.rs:905-950`) pero sin filtro por tipo de pista ni posición, sin Recientes y sin presets dentro (`collect_all_entries`, `:568`) [V] |
| 13 | **Device Slots** en el mixer: lista de dispositivos por pista, añadir, mover, desactivar, abrir plugin (`Opt+Cmd+C`) | ver y tocar la cadena de todas las pistas a la vez | el mixer muestra instrumento y efectos (`views/mixer.rs:157-166`) [V]; sin insertar/mover ni teclado [S sobre el alcance exacto] |
| 13 | **Step Sequencer** como tipo de clip: filas con longitud, rate, dirección y swing propios; pestañas Velocity/Offset/Length/Repeat/Pitch/Chance; conversión a notas y vuelta (`Cmd+Shift+M`/`P`) | polirritmia por fila dentro de un clip | `views/steps.rs` (228 líneas) solo para Drum Rack, sin longitud/rate por fila [V] |
| 13 | **Macro Mappings**: curva por breakpoints por mapeo, hasta 32 macros, modo Mod (offset) además de Remote, arrastrar el nombre de la macro al parámetro para mapear | mapeo directo por arrastre | 16 macros y variaciones; mapeo pendiente (20, trabajo 2.8) |
| 13 | **Envelope Clips en retornos**, **Mid-Side y Multiband Rack**, **Clipper**, **Transient Shaper**, **Drum Synth**, **Looplet** | — | Clipper, Transient y M/S ya existen (13 §3); el resto está en 18/21 |
| 13 | Mixer: botón **Unsolo/Uncue** global, **sends como sliders**, menú **Configure Mixer** | configurar secciones del mixer | sin botón de unsolo global [S] |
| 13 | **Duplicate Without Content** en la cabecera de pista; exportar **mapa de tempo y compases** como MIDI; MIDI exportado a 480 PPQ | — | sin export de mapa de tempo [S] |
| 13 | **ARA 2** en el Arrangement con navegador ARA contextual en la Clip View | — | fuera de alcance (host) |
| Push 3 / Move / Note | Push 3 con Live 13: **Custom Layouts** (pads mezclando lanzar clips, tocar y XYZ), **Performance Mixer**, Step Sequencer en pads ([blog Live 13](https://www.ableton.com/en/blog/live-13-is-coming/)); Move y Note 2.0 añaden pistas de audio, grabación y warp, y envían por **Link Audio** ([blog 12.4](https://www.ableton.com/en/blog/live-12-4-is-coming/)) | el hardware empuja a que todo se edite con pocos controles y con foco | sin superficies de control (13 §1.9); Link sigue bloqueado por licencia (PLAN §7) |

Lectura: Live 13 no cambia el modelo del programa; quita pasos. Capture, navegador contextual, device slots y "Duplicate Without Content" son atajos sobre funciones que ya existían. Ese es exactamente nuestro hueco.

---

## 3. Patrón por patrón: qué hace Live, qué tenemos, qué falta

### 3.1 Navegador: filtros por etiquetas, Quick Tags, colecciones y búsquedas guardadas

![Disposición del navegador de Live 12: barra lateral con Collections (colores), Library y Places; Filter View con grupos de etiquetas (Sounds → Mallets, Character → Digital); barra Results con "2 Filters", Clear y Add Label; lista de resultados; Quick Tags debajo; y la tira de preview con el botón Raw. Figura "The Browser's Layout" de https://www.ableton.com/en/live-manual/12/working-with-the-browser/](img/03-live12-browser-layout.png)

**Qué hace Live.** El navegador es un panel permanente con tres zonas: barra lateral (Collections de color, Library por tipo, Places por origen), **Filter View** con grupos de etiquetas que se combinan (y se pueden ocultar por etiqueta), y la lista. Desde 12.2 la lista tiene **columnas configurables, reordenables y ordenables**; la barra **Results** dice cuántos filtros hay y permite **guardar la búsqueda como etiqueta propia** (*Add Label*); **Quick Tags** muestra y añade etiquetas al elemento seleccionado (12.4: también crea etiquetas nuevas); **Collections** asigna colores con las teclas 1-7; el **preview** arranca en el siguiente compás con el transporte en marcha salvo que actives **Raw** ([manual §4](https://www.ableton.com/en/live-manual/12/working-with-the-browser/), [notas 12.2](https://www.ableton.com/en/release-notes/live-12/)) [V].

**Qué tenemos.** La paleta Cmd+K con pestañas (All, Samples, In key, Devices, Presets, Commands, Packs), chips de categoría con recuento, orden y columnas Waveform/Time/Key/BPM (captura `docs/screenshots/05-sample-browser.png`) y el panel de Sonidos acoplable a izquierda o derecha (`views/sounds/mod.rs`, `keys.rs:264`). Favoritos, etiquetas de usuario y carpetas virtuales viven en `crates/library` (13 §1.8) [V].

**Qué falta.** (1) **Guardar una búsqueda** (texto + chips) como colección con nombre en el panel de Sonidos: es lo que convierte "busqué kicks en La menor a 124" en un sitio al que volver. (2) **Colecciones de color con teclas 1-7** desde la lista. (3) **Quick Tags** editables en la fila seleccionada (añadir con autocompletado, quitar con X) en vez de solo verlas como chips. (4) **Historial atrás/adelante** de búsquedas (`Cmd+[`/`Cmd+]` en Live) [V]. (5) Un **conmutador Raw** explícito en el panel de Sonidos; hoy el ajuste al tempo es implícito [S sobre la UI exacta].

### 3.2 Similitud: buscar y **sustituir** por parecido

![Resultados de "Show Similar Files": la muestra de referencia queda en el campo de búsqueda y debajo la lista ordenada de más a menos parecida. Figura "Similarity Search Results" de https://www.ableton.com/en/live-manual/12/live-concepts/](img/03-live12-similarity-search.png)

**Qué hace Live.** Además de buscar parecidos (`Cmd+Shift+F`), Live 12 deja **cambiar la muestra por la siguiente más parecida dentro del dispositivo**: en Simpler con dos flechas o `Cmd+←/→`, y en Drum Rack para un pad o **para todos los pads a la vez**, con **candado por pad** y "volver a la referencia" (`Cmd+↓`) ([notas 12.0, "Similar Sample Swapping"](https://www.ableton.com/en/release-notes/live-12/)) [V].

**Qué tenemos.** "Similar" en la paleta (`views/palette/mod.rs:63, 205`) con embedding DSP (PLAN F4 ✔) [V].

**Qué falta.** **Similar Sample Swapping** en `instrument.sampler` y en los pads del Drum Rack (`views/devices.rs`, `views/device_ui/sampler.rs`): es el gesto que más acelera elegir una batería y aprovecha un índice que ya tenemos. Cada cambio es un `Command` (reemplazar la fuente del pad) y deshace en un paso.

### 3.3 Navegador contextual desde la cadena de dispositivos (Live 13)

![Navegador contextual de Live 13: se abre desde la cadena, con buscador "Search Devices & Presets", conmutadores de tipo, filtros de colección, "Recents" con los 8 últimos y después la lista por fabricante. Figura "Contextual Browsing" de https://www.ableton.com/en/release-notes/live-13-beta/](img/03-live13-contextual-browsing.png)

**Qué hace Live.** El "+" de la cadena (o doble clic entre dos dispositivos) abre una lista **ya filtrada por la posición**: antes de un instrumento solo ofrece efectos MIDI; en una pista de audio, solo efectos de audio. Arriba, **Recientes** (8); `Enter` sobre un dispositivo abre **sus presets agrupados por etiqueta**; `Opt+↑/↓` recorre presets sin abrir nada; `Cmd+←/→` mueve el dispositivo seleccionado; todo tiene atajo ([notas Live 13](https://www.ableton.com/en/release-notes/live-13-beta/)) [V].

**Qué tenemos.** El menú "ADD DEVICE" con buscador (`views/menus.rs:905-950`), abierto desde el botón de la cadena (`views/devices.rs:82`), y un menú de presets aparte (`views/menus.rs:25`, `MenuKind::Presets`) [V]. `collect_all_entries` (`views/menus.rs:568`) junta todos los dispositivos sin mirar el tipo de pista ni la posición [V].

**Qué falta.** Filtro por `TrackKind` y por posición (antes/después del instrumento), sección **Recientes**, presets en la misma lista con `Enter`, recorrido de presets con `Alt+↑/↓` sobre el dispositivo seleccionado, inserción en el hueco donde se hizo doble clic, y mover dispositivos con `Cmd+←/→`. Es trabajo de interfaz sobre datos que existen.

### 3.4 Info View, Learn View y foco de teclado

![La Info View de Live: al pasar el ratón sobre un control muestra su nombre y una explicación; se abre y cierra con "?" y admite textos propios por pista, clip o dispositivo ("Edit Info Text"). Figura de https://www.ableton.com/en/live-manual/12/first-steps/](img/03-live12-info-view.png)

**Qué hace Live.** Una franja fija en la esquina inferior izquierda que **describe lo que hay bajo el ratón** (nombre y función), con `?` para mostrarla u ocultarla, y la posibilidad de escribir **notas propias** en pistas, clips y dispositivos ([manual §2.2.2](https://www.ableton.com/en/live-manual/12/first-steps/)) [V]. Además, desde 12.0 casi toda la interfaz se recorre con teclado: menú **Navigate**, `Alt+0…7` para saltar de zona (barra, Session, Arrangement, Clip, Devices, navegador…), `Tab` para pasar de control en control si se activa "Use Tab to Move Focus" ([notas 12.0, "Keyboard Navigation"](https://www.ableton.com/en/release-notes/live-12/)) [V].

**Qué tenemos.** Tooltips por control (`widgets.rs:87-95`, 223 usos de `tooltip` en `app/src`) y la hoja de atajos con buscador. En nuestro mapa `?` abre la hoja de atajos (`keys.rs:270`) y `Tab` cambia de vista ("ToggleView" en `keys.rs`) [V]. No hay notas de usuario por pista/clip ni foco de teclado por zonas [V, por ausencia en `keys.rs`].

**Qué falta.** (1) Una **franja de ayuda** en la barra de estado (`workspace.rs:2745`, `render_status`) que muestre el texto del tooltip o el `help` de la fila de `keys::TABLE` del control bajo el ratón; reutiliza textos que ya están escritos (cada `Entry` de `keys.rs` tiene `help`). Es barato y es lo que más reduce la curva de aprendizaje. (2) **Notas de pista/clip** (campo de texto en el modelo, editado con un `Command`). (3) **Saltos de zona** con modificador (hoy `1…4` cambian la pestaña inferior sin modificador, `keys.rs`). Learn View no es prioritario: nuestro asistente ya responde "¿cómo hago X?".

### 3.5 Clip y Devices apilados

![Clip View y Device View abiertas a la vez, una encima de otra, con los triángulos junto a los selectores para mostrar cada una. Figura "Stacked Clip and Device View" de https://www.ableton.com/en/live-manual/12/live-concepts/](img/03-live12-clip-device-apiladas.png)

**Qué hace Live.** Desde 12.0 se pueden ver **a la vez** la Clip View y la Device View (`Cmd+Alt+3` y `Cmd+Alt+4`, o `Alt+clic`), y `Shift+Tab` mueve el foco entre ambas ([notas 12.0](https://www.ableton.com/en/release-notes/live-12/)) [V].

**Qué tenemos.** El panel inferior muestra una sola pestaña de `BottomTab { Devices, PianoRoll, Steps, Mixer }` (`state.rs:199-204`) y `Shift+Tab` alterna entre Devices y Clip (`keys.rs:255`) [V]. Se ve en `docs/screenshots/04-device-view.png` y `08-automation-lanes.png`: o el sinte o el piano roll, nunca los dos.

**Qué falta.** Poder abrir **Clip encima de Devices** dividiendo el alto del panel (`workspace.rs`, `bottom_h`), con un divisor arrastrable. Es la forma normal de escribir una línea y retocar el filtro del sinte que la toca sin cambiar de pestaña cada diez segundos.

### 3.6 Editor MIDI: varios clips, Focus Mode, Chance y Find & Select

![Edición multi-clip en el editor MIDI de Live 12: una barra de bucle por clip, del color del clip, encima de la rejilla; las notas de todos los clips se ven juntas. Figura "Multi-Clip Loop Bars in the MIDI Note Editor" de https://www.ableton.com/en/live-manual/12/editing-midi/](img/03-live12-multiclip-focus.png)

**Qué hace Live.** Con hasta **8 clips seleccionados** (de varias pistas o escenas) el editor muestra todas sus notas; cada clip tiene su **barra de bucle**; **Focus Mode** (`N`, o mantener `N` un momento) deja editar solo uno y pinta los demás en gris. Bajo las notas hay carriles de **Velocity y Chance**, con **grupos de probabilidad** "Play All / Play One" (`Cmd+G`). **Find and Select Notes** filtra por altura, tiempo, chance, cuenta (cada n-ésima), duración, escala y velocity ([manual §10.5.3, §10.5.13, §10.8](https://www.ableton.com/en/live-manual/12/editing-midi/)) [V]. Para comparar: Bitwig 6 edita **expresiones directamente sobre las notas** y añade "layered editing" para ver varias pistas ([historia de Bitwig 6](https://www.bitwig.com/stories/on-another-level-bitwig-studio-6-is-out-now-416/)) [V].

**Qué tenemos.** El piano roll trabaja con un solo `selected_clip` (`views/piano_roll.rs:26`, `state.rs:319`); carril de velocity (`piano_roll.rs:265`); la probabilidad solo tiñe la nota (`:239`) [V]. Hay plegado a escala y resaltado (13 §1.5).

**Qué falta.** (1) **Carril Chance** junto al de velocity, con edición por arrastre y por teclado (↑/↓ ±10 %), y grupos Play All/Play One (el motor ya reproduce `probability`, 13 §1.5). (2) **Multi-clip** con barras de bucle y Focus Mode: en electrónica es como se encaja bajo y bombo, o la variación de un patrón entre escenas. (3) **Find & Select** con al menos "cada n-ésima", "por altura" y "por escala": alimenta a todas las herramientas (humanizar solo los hats, bajar velocity a los contratiempos).

### 3.7 MIDI Tools: panel con parámetros y "Auto Apply"

![Generador "Stacks" de Live 12 en el panel Generate de la Clip View: progresión de acordes por pads, con parámetros editables. Figura de https://www.ableton.com/en/live-manual/12/midi-tools/](img/03-live12-midi-tools-stacks.png)

**Qué hace Live.** Los paneles **Transform** (Arpeggiate, Chop, Connect, Glissando, LFO, Ornament, Quantize, Recombine, Span, Strum, Time Warp, Velocity Shaper) y **Generate** (Rhythm, Seed, Shape, Stacks, Euclidean) tienen **interfaz con parámetros**. Con **Auto Apply** activado, cada cambio de parámetro rehace la transformación al instante sobre la selección (o el bucle); al desactivarlo, se ajusta y se pulsa Apply (`Cmd+Enter`). Undo solo deshace notas, no parámetros; **Reset** vuelve la herramienta a fábrica. Todas son **conscientes de la escala** (grados en lugar de semitonos) y se amplían con Max for Live ([manual §11.1](https://www.ableton.com/en/live-manual/12/midi-tools/)) [V].

**Qué tenemos.** Un menú de acciones sin parámetros: construir triada/7ª/9ª/sus, invertir, progresión de 4 compases, arpegiar, strum, chop, randomize (`views/menus.rs:352-460`) y las funciones de `state.rs:1088-1193` [V]. Además el motor de teoría de `crates/ai` y el generador de partes (PLAN F6) [V].

**Qué falta.** Convertir ese menú en un **panel lateral del piano roll** con selector de herramienta y sus parámetros, previsualización en vivo como **propuesta** (el sistema de propuestas aceptar/descartar como un paso de undo ya existe, PLAN F5) y un botón Aplicar. Empezar por las que más se usan en electrónica: **Rhythm/Euclidean** (algoritmo de Bjorklund; [Toussaint 2005, paper](http://cgm.cs.mcgill.ca/~godfried/publications/banff.pdf) [V]), **Arpeggiate**, **Strum**, **Quantize con fuerza/swing**, **Velocity Shaper** y **Stacks** (que ya cubre a medias la progresión de `state.rs:1088`). Licencia: implementación propia a partir del paper y del manual; no hay código de Ableton que mirar.

### 3.8 Arrangement: locators, pistas enlazadas y "Lock Envelopes"

![Locators en la regla del Arrangement de Live: marcas con nombre que lanzan la reproducción desde ese punto. Figura "Use Locators to Launch Playback in the Arrangement" de https://www.ableton.com/en/live-manual/12/arrangement-view/](img/03-live12-locators.png)

**Qué hace Live.** **Locators** en la regla: se ponen en directo con *Set Locator* (cuantizados a la cuantización global), se renombran (`Cmd+R`), se saltan con anterior/siguiente o con mapeo MIDI/teclado, y tienen "Loop to Next Locator" y "Set Song Start Time Here". **Linked-Track Editing** aplica cortes, fades, comping y movimientos a varias pistas enlazadas (batería multipista). **Lock Envelopes** desacopla la automatización de los clips al moverlos ([manual §6.4, §6.14](https://www.ableton.com/en/live-manual/12/arrangement-view/), [§25.5.6](https://www.ableton.com/en/live-manual/12/automation-and-editing-envelopes/)) [V].

**Qué tenemos.** Regla con bucle (captura `01-arrangement.png`) y take lanes; nada de locators en modelo ni UI [V].

**Qué falta.** `Project.locators` con `Command`s de añadir/mover/renombrar/borrar, pintado en la regla, "Loop to Next Locator" y teclas de salto. Es **lo más bloqueante del arrangement** para terminar una canción: sin marcas de intro/drop/break no hay estructura visible y no se puede ensayar una sección.

### 3.9 Automatización: formas, asas, simplificar, teclado y Capture

![Formas de automatización de Live 12 en el menú de una selección de tiempo: fila superior de ondas (seno, triángulo, sierra, sierra inversa, cuadrada) y fila inferior de rampas y ADSR que se enlazan con el valor existente. Figura "The Two Rows Contain Different Types of Automation Shapes" de https://www.ableton.com/en/live-manual/12/automation-and-editing-envelopes/](img/03-live12-automation-shapes.png)

**Qué hace Live.** Sobre una **selección de tiempo** en una lane: **insertar formas** (ondas y rampas/ADSR), **asas** en los bordes para escalar en vertical, estirar en horizontal y **sesgar** desde las esquinas, y **Simplify Envelope** para limpiar una grabación. Desde 12.2, **edición completa por teclado** (Enter crea/selecciona punto, flechas mueven, teclear cambia el valor, Tab salta de punto, `Alt+↑/↓` cambia de parámetro automatizado). En 13, **Capture Automation** ([manual §25.5](https://www.ableton.com/en/live-manual/12/automation-and-editing-envelopes/), [notas 12.2](https://www.ableton.com/en/release-notes/live-12/), [notas 13](https://www.ableton.com/en/release-notes/live-13-beta/)) [V].

![Bitwig Studio 6: clips de automatización como bloques con nombre (Main, Peak, Mellow, Motion…) bajo los clips de notas, también en el lanzador de clips. Figura de https://www.bitwig.com/stories/on-another-level-bitwig-studio-6-is-out-now-416/](img/03-bitwig6-automation-clips.png)

**Quién lo hace mejor: Bitwig 6.** Convierte la automatización en **clips**: se repiten, se hacen bucle, se estiran, se guardan en la librería y siguen al clip de notas que tienen encima ("Follow") o se quedan fijos ("Lock", tecla `0`). Además: puntos con **spread** (valor aleatorio dentro de un rango en cada pasada) y **hold** (escalón), lápiz que convierte el trazo en curvas limpias, **Spray Can** que pinta puntos a la rejilla, y **clip aliases** (clips que comparten patrón) ([historia de Bitwig 6](https://www.bitwig.com/stories/on-another-level-bitwig-studio-6-is-out-now-416/), [notas 6.0, 11-mar-2026](https://downloads.bitwig.com/6.0/Release-Notes-6.0.html)) [V]. El PLAN §7.1 punto 13 ya pedía "clips de automatización"; Bitwig enseña la forma correcta.

**Qué tenemos.** Lanes con selector Device → Parameter y búsqueda (`views/arrangement.rs:1531`, `render_chooser`), puntos, curvas, rango y desplazamiento de rango (13 §1.7), Automation Arm limitado a ventanas de plugin (13 §1.2) [V].

**Qué falta, por orden.** (1) **Formas sobre selección** (seno/triángulo/sierra/cuadrada a la rejilla, rampas y ADSR): es la mitad del trabajo de un build-up. (2) **Asas de escalado/sesgo** sobre la selección. (3) **Simplify** con Ramer-Douglas-Peucker ([descripción del algoritmo](https://en.wikipedia.org/wiki/Ramer%E2%80%93Douglas%E2%80%93Peucker_algorithm) [V]; que Ableton use ese mismo algoritmo es [S]). (4) **Capture Automation**: un búfer circular en la app con los últimos segundos de cambios de parámetro (ya pasan por `Command`s) y un botón que los escribe como lane. (5) **Clips de automatización** estilo Bitwig, más adelante y sobre el modelo de `Source`, que ya admite clips enlazados (`model/src/project.rs:4-5`).

### 3.10 Mixer en el Arrangement y device slots

![Mixer de Live 12 abierto bajo el Arrangement, con las "Arrangement Track Controls" a la derecha de las cabeceras. Figura "The Mixer (Bottom) and Arrangement Track Controls (Right)" de https://www.ableton.com/en/live-manual/12/arrangement-view/](img/03-live12-mixer-arrangement.png)

![Device Slots de Live 13: una sección del mixer que lista los dispositivos de cada pista (con LED de activación, llave para abrir el plugin y "+" para insertar con el navegador contextual), sobre los envíos y los faders. Figura de https://www.ableton.com/en/release-notes/live-13-beta/](img/03-live13-device-slots.png)

**Qué hace Live.** El mixer se abre en cualquiera de las dos vistas (`Cmd+Alt+M`), es redimensionable (al crecer añade escala en dB, campo numérico y picos reseteables) y **cada sección se enciende o apaga** (I/O, Sends, Returns, Volume, Track Delay, Crossfader, **Performance Impact** con 6 segmentos de CPU por pista). Live 13 añade **Device Slots**, sends como sliders y Unsolo global ([manual §18](https://www.ableton.com/en/live-manual/12/mixing/), [notas 13](https://www.ableton.com/en/release-notes/live-13-beta/)) [V]. FL Studio 2025 permite **añadir o quitar pistas del mixer** hasta 500 ([novedades 2025](https://www.image-line.com/fl-studio/release/2025)) [V]; en nuestro modelo eso ya es natural porque mixer = pistas.

**Qué tenemos.** Mixer en el panel inferior que se adapta al alto (`workspace.rs:2918`, `views/mixer.rs:68-98`), con nombre de instrumento y efectos por tira (`mixer.rs:157-166`) y routing (§1) [V]. Las filas de retorno del Arrangement dicen "1 effect" (captura `01-arrangement.png`).

**Qué falta.** (1) **Menú "Configurar mixer"** para mostrar u ocultar secciones. (2) **Device slots interactivos**: activar/desactivar, reordenar arrastrando, doble clic que abre el dispositivo en el panel, "+" con el navegador contextual de §3.3. (3) **Impacto de CPU por pista**: el motor ya mide la carga global (CPU en `topbar.rs`); medir por nodo y mostrarlo ayuda a decidir qué congelar. (4) **Unsolo** global.

### 3.11 Bounce: en sitio, a pista nueva, de grupo y "Paste Bounced Audio"

![Comandos de Bounce de Live 12.2 en el menú contextual: Bounce Track in Place y Bounce to New Track. Figura de https://www.ableton.com/en/release-notes/live-12/ (sección 12.2)](img/03-live122-bounce.png)

**Qué hace Live.** **Bounce Track in Place** renderiza la pista entera a audio (sustituye al antiguo "Freeze and Flatten"); **Bounce to New Track** (`Cmd+B`) renderiza una selección de tiempo a una pista nueva debajo y silencia el origen; 12.3 añade **Bounce Group** y **Paste Bounced Audio** (copias clips o un rango y pegas el audio ya procesado donde quieras, también en una take lane). Los archivos van a `Samples/Processed/Bounce` del proyecto ([notas 12.2 y 12.3](https://www.ableton.com/en/release-notes/live-12/)) [V].

**Qué tenemos.** Freeze, unfreeze y flatten de pistas (`app/src/freeze.rs:116-153`) y el render offline de consolidar (13 §1.4) [V]. Ningún comando Bounce (`grep` vacío) [V].

**Qué falta.** **Bounce to New Track** sobre la selección de tiempo y **Paste Bounced Audio**: reutilizan el render de `freeze.rs` limitado a un rango. Es el gesto de remezclar en electrónica (resamplear un fill, invertir una cola de reverb) y hoy exige exportar e importar.

### 3.12 Step Sequencer como tipo de clip (Live 13)

![Step Sequencer de Live 13: filas por pad con su propia longitud (16, 7, 12 pasos), rate, dirección y swing; pestañas inferiores Velocity, Offset, Length, Repeat, Pitch y Chance. Figura de https://www.ableton.com/en/release-notes/live-13-beta/](img/03-live13-step-sequencer.png)

**Qué hace Live.** Un clip de pasos (8 a 128) donde **cada fila tiene longitud, rate, dirección y swing propios** (polirritmia sin trucos), con pestañas por paso para velocity, offset, longitud, repeticiones, pitch y chance; se crea con `Cmd+Shift+P` o con doble clic en un slot vacío de una pista con Drum Rack; se convierte a notas y de vuelta ([notas 13](https://www.ableton.com/en/release-notes/live-13-beta/)) [V].

**Qué tenemos.** `views/steps.rs` (228 líneas) para el Drum Rack, con zoom anclado al puntero (`:56`); sin longitud ni rate por fila [V].

**Qué falta.** Longitud y rate por fila y las pestañas **Chance/Repeat** por paso. No hace falta un tipo de clip nuevo: basta con que el step sequencer escriba notas con `probability` y repeticiones expandidas, y guarde la configuración de fila en el clip. Prioridad media: el piano roll ya cubre lo esencial.

### 3.13 Escalas y acordes: Live, Bitwig y Logic

![Chord Track de Logic Pro: pista global de acordes (Am, F, C, G, Am7(13)…) que siguen los Session Players de bajo y teclado; los acordes por región se ven debajo de cada región. Figura de https://support.apple.com/guide/logicpro/chords-overview-lgcp2633963f/mac](img/03-logic-chord-track.png)

**Qué hace cada uno.** Live 12 hace la escala **por clip**, la refleja en la barra y la propaga a MIDI Tools y a efectos MIDI con "Use Current Scale" ([manual §3.13](https://www.ableton.com/en/live-manual/12/live-concepts/)) [V]. Bitwig 6 añade una **armadura de proyecto** que se puede **automatizar** y que siguen las note FX con "Use Global Key", más **Snap to Key** (`K`) en el lápiz ([historia de Bitwig 6](https://www.bitwig.com/stories/on-another-level-bitwig-studio-6-is-out-now-416/)) [V]. Logic tiene la **Chord Track global**: los Session Players (bajo, teclado) siguen los acordes, que también pueden ir por región; al arrastrar una región a la Chord Track, Logic analiza sus acordes ([guía de Logic](https://support.apple.com/guide/logicpro/chords-overview-lgcp2633963f/mac)) [V]. La guía de Apple ya lista **Logic Pro para Mac 13.0** como versión actual [V, misma página]; aquí se mira la función, que llegó con Logic Pro 11 ([nota de prensa de Apple, 7-may-2024](https://www.apple.com/newsroom/2024/05/logic-pro-takes-music-making-to-the-next-level-with-new-ai-features/)) [V].

**Qué tenemos.** Escala global/pista/clip con plegado y resaltado (13 §1.5), detección de acordes en `crates/ai/src/chords.rs:173` y `:314` (`chords_per_bar`) y el generador de partes (PLAN F6). Falta la **Chord Track en la interfaz** (PLAN F6 ✘) [V].

**Qué falta.** Una **pista global de acordes** encima de las pistas (como la de tempo), que alimente el generador de partes y los efectos MIDI (`midi.chord`, `midi.arp`) y que se pueda rellenar arrastrando un clip (con `detect_chords`). Aquí Logic va por delante de Live y nosotros ya tenemos la mitad del motor.

### 3.14 Otros patrones que conviene copiar (sin captura)

| Patrón | Qué hace (fuente) | Nuestro estado | Valor |
|---|---|---|---|
| **Undo History** | lista de pasos para volver a cualquier punto (manual Live §5.4.2, en el índice del [manual](https://www.ableton.com/en/live-manual/12/arrangement-view/)) [V] | solo la etiqueta "Undo: …" (`workspace.rs:2747`) | medio |
| **Capture MIDI** | recupera lo tocado sin grabar ([manual §3.1](https://www.ableton.com/en/live-manual/12/live-concepts/)) [V] | ausente | alto para tocar ideas |
| **Follow actions con UI** | dos acciones con probabilidad; 12.2: scene follow "Longest" ([notas 12.2](https://www.ableton.com/en/release-notes/live-12/)) [V] | modelo sí, UI no (§1) | medio |
| **Momentary latching** | mantener `A`, `B`, `S`, `Z` o `Tab` alterna mientras se pulsa ([notas 12.0](https://www.ableton.com/en/release-notes/live-12/)) [V] | no (`keys.rs`) | bajo coste, alto uso |
| **Shift + letra con teclado MIDI activo** | los atajos de una letra siguen vivos con Shift ([notas 12.0](https://www.ableton.com/en/release-notes/live-12/)) [V] | la tecla `M` activa el teclado del ordenador (`keys.rs`) [V]; conflicto no resuelto [S] | medio |
| **Move Insert Marker to Playhead** | `Cmd+Shift+Space` ([notas 12.0](https://www.ableton.com/en/release-notes/live-12/)) [V] | ausente en `keys.rs` | bajo |
| **Duplicate Without Content** | copia la pista sin clips ([notas 13](https://www.ableton.com/en/release-notes/live-13-beta/)) [V] | ausente | bajo |
| **A/B de dispositivo** | dos estados por dispositivo (12.3, [notas 12](https://www.ableton.com/en/release-notes/live-12/)) [V] | ausente (`grep compare` vacío) | medio |
| **Stretch/pitch/reverse por clip en propiedades** | FL Studio 2025 lo añade en "Clip Properties" ([novedades 2025](https://www.image-line.com/fl-studio/release/2025)) [V] | ya lo tenemos en el panel de clip (13 §1.4) | — |
| **Loop Starter** | FL 2025: pila de loops por género para empezar ([novedades 2025](https://www.image-line.com/fl-studio/release/2025)) [V] | el generador de partes y el panel de Sonidos podrían ofrecerlo | medio |
| **Arranger Auto Zoom** | Bitwig 6 agranda la pista seleccionada sin mover el resto ([historia de Bitwig 6](https://www.bitwig.com/stories/on-another-level-bitwig-studio-6-is-out-now-416/)) [V] | alto de pista por pista (`views/arrangement.rs:95`) | medio |

---

## 4. Comparación por zona

| Zona | Live 12.4/13 | Mejor que Live | AI-Track | Brecha |
|---|---|---|---|---|
| Navegador | panel fijo, etiquetas, Quick Tags, colecciones, búsquedas guardadas, similitud con sustitución, contextual en la cadena (13) | — | Cmd+K + panel de Sonidos con preview a tempo; similitud; Splice | media: guardar búsquedas, colecciones, sustituir por parecido, contextual |
| Ayuda | Info View, Learn View | — | tooltips y hoja de atajos | media, barata |
| Clip View MIDI | multi-clip, Focus, Chance, grupos, Find & Select, MIDI Tools con parámetros | Bitwig: expresiones sobre la nota | un clip, velocity, menú de acciones | **alta** |
| Arrangement | locators, linked tracks, take lanes, lock envelopes | Bitwig: auto zoom, alias | take lanes, rejilla adaptativa | **alta** (locators) |
| Automatización | formas, asas, simplificar, teclado, Capture (13) | Bitwig 6: clips, spread/hold, spray can | lanes y selector | **alta** |
| Mixer | en ambas vistas, secciones configurables, CPU por pista, device slots (13) | FL: añadir/quitar pistas | mixer adaptable con routing | media |
| Render | Bounce en sitio, a pista nueva, de grupo, pegar | — | freeze/flatten | **alta** |
| Armonía | escala por clip y en dispositivos | Logic: Chord Track + Session Players; Bitwig: armadura automatizable | escala global/pista/clip, detección de acordes | media (Chord Track) |
| Teclado | Navigate, foco por zonas, latching | — | tabla única de atajos | media |

---

## 5. Carencias ordenadas por lo que más bloquea hacer una canción

Criterio de PLAN §7.1: ir de un proyecto vacío a una canción de 3 minutos sin pelear con la interfaz.

1. **Locators y estructura de la canción** (§3.8): sin ellos no se ve ni se ensaya la forma.
2. **Automatización rápida** (§3.9): formas, asas y Capture; un build-up hoy se dibuja punto a punto.
3. **Bounce a pista nueva / Paste Bounced Audio** (§3.11): resamplear es medio flujo de la electrónica.
4. **Clip + Devices apilados** (§3.5): cada cambio de sonido mientras se escribe cuesta un cambio de pestaña.
5. **Carril Chance y multi-clip con Focus** (§3.6): variación y encaje bajo-bombo.
6. **MIDI Tools con parámetros y previsualización** (§3.7): pasar de "acción ciega" a "ajusto hasta que suena".
7. **Navegador contextual y device slots** (§3.3, §3.10): insertar y ver efectos sin salir del sitio.
8. **Sustitución por similitud en Sampler y Drum Rack** (§3.2): elegir batería por oído.
9. **Info View en la barra de estado** (§3.4): curva de aprendizaje.
10. **Chord Track** (§3.13): armonía compartida para el generador y los efectos MIDI.
11. Guardar búsquedas, colecciones y Quick Tags (§3.1); Undo History; latching; follow actions con UI; step sequencer por fila (§3.12, §3.14).

---

## 6. Lista priorizada de mejoras

Riesgo de licencia: todas son implementación propia de un comportamiento descrito en manuales y notas públicas; no se copia código ni gráficos. Las figuras de este informe no van al producto.

| # | Qué cambiar | Dónde | Algoritmo o referencia | Esf. | Impacto | Riesgo de licencia |
|---|---|---|---|---|---|---|
| 1 | **Locators**: `Project.locators`, `Command`s Add/Move/Rename/Remove, pintado y arrastre en la regla, "Set" cuantizado, anterior/siguiente, "Loop to Next Locator", "Start Here", salto por teclado | `model/src/project.rs`, `model/src/command.rs`, `app/src/views/arrangement.rs` (regla), `app/src/keys.rs` | manual Live §6.4 [V] | S-M | **Alto de uso**: estructura visible y ensayo de secciones | ninguno |
| 2 | **Formas de automatización** sobre selección (seno, triángulo, sierra, cuadrada a la rejilla; rampas y ADSR enlazados), **asas** de escala/estiramiento/sesgo y **Simplify** | `app/src/views/arrangement.rs` (lanes), `app/src/editing/` (gestos), `model/src/automation.rs` | manual §25.5.3-25.5.5 [V]; Ramer-Douglas-Peucker [V] | M | **Alto audible**: build-ups, gates y barridos de filtro en minutos | ninguno (algoritmo público) |
| 3 | **Bounce to New Track** (`Cmd+B` sobre la selección) y **Paste Bounced Audio**; después Bounce Group | `app/src/freeze.rs` (render a rango), `app/src/state.rs`, `app/src/keys.rs`, `app/src/editing/clipboard.rs` | notas Live 12.2/12.3 [V] | M | **Alto audible**: resampleo sin exportar | ninguno |
| 4 | **Capture Automation y Capture MIDI**: búfer circular de los últimos ~30 s de cambios de parámetro y de notas tocadas; botón en la barra que los escribe como lane/clip en un paso de undo | `app/src/state.rs` (registro de `Command`s de parámetro), `app/src/views/topbar.rs`, `app/src/audition.rs` (notas) | notas Live 13 [V] | M | **Alto de uso**: no se pierde una buena toma | ninguno |
| 5 | **Clip y Devices apilados** con divisor arrastrable; `Shift+Tab` mueve el foco entre ambos | `app/src/state.rs:199` (`BottomTab` → conjunto de vistas visibles), `app/src/workspace.rs` (`bottom_h`), `app/src/views/bottom.rs` | notas Live 12.0 [V] | S-M | **Alto de uso** | ninguno |
| 6 | **Carril Chance** y grupos Play All/Play One; ↑/↓ ±10 % por teclado | `app/src/views/piano_roll.rs` (junto a `:265`), `model/src/project.rs` (grupo de probabilidad en la nota), `engine/src/clip.rs` (Play One) | manual §10.5.13 [V] | S-M | **Alto audible**: variación en hats y percusión | ninguno |
| 7 | **Panel de MIDI Tools** con selector, parámetros y Auto Apply como **propuesta** (aceptar/descartar); primeras: Rhythm/Euclidean, Arpeggiate, Strum, Quantize con fuerza/swing, Velocity Shaper, Stacks | nuevo `app/src/views/midi_tools/`; reutiliza `state.rs:1088-1193` y la teoría de `crates/ai` | manual §11 [V]; Bjorklund/Toussaint 2005 [V] | M-L | Alto de uso | ninguno |
| 8 | **Multi-clip + Focus Mode** en el piano roll (hasta 8 clips, barras de bucle, `N`) | `app/src/views/piano_roll.rs:26` (de `selected_clip` a lista), `app/src/state.rs:319` | manual §10.8 [V] | M-L | Medio-alto | ninguno |
| 9 | **Navegador contextual** en la cadena: filtro por `TrackKind` y posición, Recientes, presets con `Enter`, `Alt+↑/↓` para recorrer presets, `Cmd+←/→` para mover el dispositivo | `app/src/views/menus.rs:568, 905` (`collect_all_entries`, `device_menu`), `app/src/views/devices.rs:82`, `app/src/keys.rs` | notas Live 13 [V] | S-M | Medio-alto | ninguno |
| 10 | **Device slots** en el mixer (activar, reordenar, abrir, "+"), menú **Configurar mixer**, **Unsolo** global y **CPU por pista** | `app/src/views/mixer.rs:145-260`, `engine/src/status.rs` (carga por nodo) | notas Live 13, manual §18.9 [V] | M | Medio | ninguno |
| 11 | **Sustituir por similitud** en Sampler y pads de Drum Rack (anterior/siguiente, todos los pads, candado, volver a la referencia) | `app/src/views/device_ui/sampler.rs`, `app/src/views/devices.rs`, `crates/library` (consulta de vecinos ya existente) | notas Live 12.0 [V] | S-M | Medio-alto audible | ninguno |
| 12 | **Franja de ayuda** (Info View) en la barra de estado con el texto del control bajo el ratón; `?` la alterna y la hoja de atajos queda en `Cmd+/`; notas de usuario por pista/clip | `app/src/workspace.rs:2745` (`render_status`), `app/src/widgets.rs:87` (registrar el texto del tooltip activo), `app/src/keys.rs:270` | manual §2.2.2 [V] | S | Medio (aprendizaje) | ninguno |
| 13 | **Chord Track** global encima de las pistas, editable y rellenable arrastrando un clip (`detect_chords`); la siguen el generador de partes y `midi.chord`/`midi.arp` | `model/src/project.rs` (pista global), `app/src/views/arrangement.rs`, `crates/ai/src/chords.rs:173, 314` | guía de Logic [V]; "Use Global Key" de Bitwig [V] | M | Medio-alto (armonía coherente) | ninguno |
| 14 | **Panel de Sonidos**: guardar búsqueda como colección, colores 1-7, Quick Tags editables, atrás/adelante, conmutador Raw | `app/src/views/sounds/`, `crates/library/src/db.rs` | manual §4 [V] | M | Medio | ninguno |
| 15 | **Teclado**: latching momentáneo (`A`, `B`, `Z`), `Cmd+Shift+Space` (marcador al playhead), Copy Time (`Cmd+Shift+C`), Undo History, Duplicate Without Content, follow actions en el panel de clip | `app/src/keys.rs`, `app/src/commands.rs`, `app/src/views/bottom.rs` | notas Live 12.0, 12.4, 13 [V] | S c/u | Medio | ninguno |
| 16 | **Step sequencer por fila** (longitud, rate, dirección) y pestañas Chance/Repeat | `app/src/views/steps.rs`, modelo de clip | notas Live 13 [V] | M | Medio | ninguno |
| 17 | **Clips de automatización** estilo Bitwig (Follow/Lock respecto al clip padre) | `model/src/project.rs` (nueva fuente de automatización), `engine/src/automation.rs` | notas Bitwig 6.0 [V] | L | Alto a largo plazo | ninguno |

Las cinco que más mueven la aguja: locators en la regla (1), formas/asas/simplify de automatización (2), Bounce a pista nueva y Paste Bounced (3), Capture de automatización y MIDI (4), y Clip+Devices apilados (5).

---

## 7. Qué no se pudo verificar

- No se ejecutó la app ni se tomaron capturas nuevas de AI-Track; el estado visual sale de `docs/screenshots/` y del código.
- Las capturas de Ableton, Bitwig y Apple son figuras de sus páginas, no pantallazos del navegador (el navegador de la plataforma necesita una máquina en la nube que no estaba disponible).
- Si Ableton simplifica envolventes con RDP u otro algoritmo es [S]; el manual no lo dice.
- Cómo trata hoy nuestra separación de stems una selección de tiempo y si el panel de Sonidos tiene un conmutador "Raw" visible no se comprobó en pantalla [S].
- Live 13 es beta: las funciones pueden cambiar hasta el lanzamiento ("early 2027").

## 8. Fuentes

- Ableton, notas de versión de Live 12 (12.0-12.4.16): https://www.ableton.com/en/release-notes/live-12/
- Ableton, notas de la beta de Live 13 (13.0b25): https://www.ableton.com/en/release-notes/live-13-beta/
- Ableton, "What's coming in Live 13" (10-nov-2026): https://www.ableton.com/en/blog/live-13-is-coming/
- Ableton, "Live 12.4 is coming": https://www.ableton.com/en/blog/live-12-4-is-coming/ · "What's new in Live 12.4": https://help.ableton.com/hc/en-us/articles/26963870210076-What-s-new-in-Live-12-4
- Ableton, notas de Push con Live 12 (beta): https://www.ableton.com/en/release-notes/push-12-beta/
- Manual de Live 12: https://www.ableton.com/en/live-manual/12/first-steps/ · https://www.ableton.com/en/live-manual/12/live-concepts/ · https://www.ableton.com/en/live-manual/12/working-with-the-browser/ · https://www.ableton.com/en/live-manual/12/clip-view/ · https://www.ableton.com/en/live-manual/12/editing-midi/ · https://www.ableton.com/en/live-manual/12/midi-tools/ · https://www.ableton.com/en/live-manual/12/arrangement-view/ · https://www.ableton.com/en/live-manual/12/automation-and-editing-envelopes/ · https://www.ableton.com/en/live-manual/12/mixing/ · https://www.ableton.com/en/live-manual/12/session-view/
- Bitwig, "On Another Level – Bitwig Studio 6 is out now": https://www.bitwig.com/stories/on-another-level-bitwig-studio-6-is-out-now-416/ · notas 6.0: https://downloads.bitwig.com/6.0/Release-Notes-6.0.html
- Image-Line, novedades de FL Studio 2025: https://www.image-line.com/fl-studio/release/2025
- Apple, guía de Logic Pro "Overview of chords": https://support.apple.com/guide/logicpro/chords-overview-lgcp2633963f/mac · nota de prensa de Logic Pro 11: https://www.apple.com/newsroom/2024/05/logic-pro-takes-music-making-to-the-next-level-with-new-ai-features/
- G. Toussaint, "The Euclidean Algorithm Generates Traditional Musical Rhythms" (paper): http://cgm.cs.mcgill.ca/~godfried/publications/banff.pdf
- Ramer-Douglas-Peucker (descripción del algoritmo): https://en.wikipedia.org/wiki/Ramer%E2%80%93Douglas%E2%80%93Peucker_algorithm
