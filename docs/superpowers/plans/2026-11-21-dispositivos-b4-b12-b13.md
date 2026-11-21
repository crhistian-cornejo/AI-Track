# Dispositivos stock B13, B12 y B4: notas de diseño

Origen: `docs/research/25-paridad-live-12-13-dispositivos.md` (§5.1 reglas de parámetros, §5.2 plantilla de ficheros, §5.4 filas B13, B12 y B4). Orden de trabajo: B13, B12, B4. Reglas que valen para las tres: `choices` mide `max − min + 1` y una lista existente no se amplía (se añade un id nuevo); ningún id existente cambia de rango ni de significado; todo id nuevo tiene un valor por defecto que deja el sonido de los presets viejos igual; nada de `gen`; sin asignaciones, locks ni E/S en `process`/`process_midi`/`modulate`; diseños y nombres propios. Los presets nuevos se escriben con generadores nuevos de `crates/stock/examples/` que solo crean los ficheros nuevos (no reescriben los viejos).

## B13 · `midi.chord`: Strum, Chance por voz y Learn

**Ids nuevos** (los 0–21 no se tocan; `ParamStore` se dimensiona por el id mayor):

| Id | Nombre | Rango | Defecto | Qué hace |
|---|---|---|---|---|
| 22–27 | `CHANCE0 + v` (Chance 1…6) | 0–100 % | 100 | Probabilidad de que la voz `v` suene en cada pulsación. |
| 28 | `STRUM` | 0–500 ms | 0 (apagado) | Tiempo entre la primera y la última nota del acorde. |
| 29 | `STRUM_DIR` | Up, Down, Down-Up, Random | Up | Orden por altura. Down-Up: baja en el pulso y sube a contratiempo (paridad de la corchea de la posición de canción). |
| 30 | `STRUM_TENSION` | −100…100 % | 0 | Curva de los retardos: `retardo = tiempo · t^(4^tensión)`; + ensancha los huecos hacia el final, − los estrecha. |
| 31 | `STRUM_VEL` | −100…100 % | 0 | Rampa de velocidad a lo largo del rasgueo (`1 + rampa · (t − ½)`). |
| 32 | `SEED` | 0–9999 | 1 | Semilla de las tiradas. |
| 33 | `LOOP` | Off, 1, 2, 4, 8 compases | Off | Las tiradas se repiten cada N compases (`PositionHash`, como `midi.echo`). |
| 34 | `LEARN` | Off, On | Off | Modo escucha. |

**Comportamiento.** Cada pulsación hace una tirada (`PositionHash` + `Draw`: semilla + posición de canción, igual en directo que al exportar) y siempre consume seis números, haya o no probabilidad < 100 %, para que tocar un chance no mueva las tiradas de las otras voces. Con `STRUM = 0` y todas las probabilidades a 100 % el camino es el de antes (notas inmediatas, mismo orden e ids): los presets y proyectos viejos suenan igual. Con rasgueo, la primera nota sale ya y las demás quedan en un arreglo fijo de pendientes (64 pulsaciones × 6 voces, sin asignar); soltar la tecla cancela las que aún no han sonado y cierra las que sí, y el reintento por tecla compartida se decide cuando la nota arranca, no cuando se pulsa. Learn: con `LEARN = On` las notas pasan tal cual (una nota por tecla) y el dispositivo recoge la unión de teclas de cada gesto (de la primera pulsación hasta soltarlas todas, máx. 6, las más graves); al terminar publica en `meter()` `[serie, n, c0…c5, n_en_vivo]`. El editor, al ver otra `serie`, escribe `SetParam` (un solo paso de deshacer): la nota más grave es el intervalo 0, las demás intervalos sobre ella con nivel 100 %, las voces sobrantes se apagan y `LEARN` vuelve a Off.

**Ficheros.** `crates/stock/src/midi/chord.rs` y `chord/tests.rs`; `crates/stock/tests/midi_realtime.rs` (se añade `midi.chord` a `PAIRED`: comprobación nota a nota con todos los presets); `crates/stock/examples/presets_chord_strum.rs` y 5 presets en `crates/stock/presets/midi.chord/`; `crates/app/src/views/device_ui/chord.rs` (celdas con Chance, tarjeta Strum con dibujo del rasgueo, tarjeta Chance con Seed y Loop, botón Learn, la cabecera deja de decir "sin medidor"). No cambian `REGISTRY`, `has_editor`/`editor()`, la paleta ni el glifo (el dispositivo ya está); el tamaño de ventana (`placement.rs`, 700 × 540) solo si la captura muestra que no cabe.
