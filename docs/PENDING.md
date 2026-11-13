# Pendiente

Trabajo pedido el 2026-11-12 que se canceló para cerrar la ejecución (sin créditos). Cada punto dice qué falta, cómo se pensaba hacer y por dónde empezar. Las reglas de `CLAUDE.md` siguen valiendo: toda edición es un `Command`, nada de asignaciones en el hilo de audio, colores de `theme.rs`, tests junto al código, nada de dependencias GPL, y **nunca `cargo fmt`** (el código está formateado a mano).

## 1. Paleta Cmd+K: plugins agrupados y etiquetas compactas

**Qué falta.** Separar los dispositivos en dos grupos, **Instrumentos** (entrada MIDI) y **Efectos de audio**, tanto los de fábrica como los VST3/CLAP instalados, con subgrupos al estilo de Live/FL:

- Efectos: EQ, Dinámica (compresor, limitador, gate, transitorios), Saturación y distorsión, Reverb, Delay, Modulación, Filtro, Pitch y tiempo, Estéreo y utilidades, Multibanda/mastering, Creativos, Análisis.
- Instrumentos: Analógicos/VA, Wavetable/digitales, FM, Sampler/pianos, Batería, Multimotor.

Además, las filas de etiquetas de la paleta deben ocupar **una sola fila** con un chip "Más (+N)" al final que despliega el resto.

**Cómo.**
- Categoría a partir de los metadatos del plugin: `features` de CLAP (`audio-effect|reverb`), subcategorías VST3 (`Fx|Reverb`, `Instrument|Synth`), y una tabla corta de correcciones para los plugins conocidos (OB-Xf → Analógicos, Dexed → FM, sfizz → Sampler, CloudReverb → Reverb, ZL Equalizer → EQ…).
- El usuario puede reasignar la categoría de un plugin; se guarda en las preferencias.
- Insignias de fabricante y formato, favoritos y usados recientemente.
- Un componente de fila de chips reutilizable: mide el ancho, muestra los que caben y añade "Más (+N)". Expandir y contraer con las curvas de `motion.rs`, sin romper la navegación por teclado.

**Dónde.** `crates/app/src/views/palette/` (pestaña Devices), `crates/app/src/views/menus.rs` (`device_menu`), `crates/app/src/bridge.rs` (`DeviceCategory`), `crates/host/src/info.rs` (metadatos de los plugins escaneados).

## 2. "Show automation" con clic derecho en los knobs

**Qué falta.** Clic derecho en cualquier knob de la vista de dispositivos o de las ventanas grandes de dispositivo → "Show automation", que abre el carril de ese parámetro en el arreglo.

**Cómo.** Reutilizar `show_automation` / `shown_lanes` (ver la memoria del proyecto "Automation lanes UI"). Añadir el menú contextual en `knob_control` (`crates/app/src/views/devices.rs`) y en los editores de `device_ui/*`, pasando `AutomationTarget::Param { device, param }`.

## 3. Rendimiento y bajo consumo

**Objetivo.** Ser el DAW más ligero: un proyecto de trap con voces, mezcla y mastering debe ir fluido en un MacBook Air o un PC Windows modesto, con Chrome/YouTube/Spotify abiertos y sin calentar el equipo.

**Cómo, en orden.**
1. **Medir primero.** Crear un proyecto de prueba de 24 pistas (audio y MIDI, synths y efectos) y medir CPU en reposo, CPU reproduciendo, RAM y tiempo de arranque. Guardar los números en `docs/research/`.
2. **Interfaz en reposo:** ningún redibujado si nada cambia. Fotogramas de animación de GPUI solo mientras algo se anima. Medidores y cabezal a menor frecuencia, y pausados con la ventana oculta o minimizada.
3. **Motor:**
   - flush de denormales;
   - SIMD en los bucles calientes;
   - saltar pistas en silencio y dispositivos inactivos según su `tail`;
   - dormir los plugins que no reciben audio.
4. **Memoria:**
   - la caché de samples no decodifica archivos largos enteros a f32: streaming o mapeo en memoria;
   - audio decodificado compartido;
   - expulsión LRU con un presupuesto configurable;
   - picos de onda guardados en disco.
5. **Plugins externos:** permitir varios plugins por proceso sandbox (configurable), en vez de un proceso por plugin.
6. **Hilos:** sin esperas activas. Trabajo de fondo en los núcleos de eficiencia de Apple Silicon.
7. **Preferencias → Rendimiento:**
   - CPU y RAM por pista y por plugin;
   - modo "Batería / bajo consumo": buffer mayor, menos refresco de medidores y vista previa más ligera.
8. Comprobar que compila en Windows: `cargo check --workspace --target x86_64-pc-windows-msvc`.

## 4. Suite propia de plugins: lo que no se llegó a hacer

Ya están hechos (o a punto, si su tarea terminó): reverb y delay, EQ dinámico y compresor, y saturación/cinta/amplificador/consola. Faltan:

- **Acid 303** (motor de Vault o instrumento propio):
  - filtro ladder de diodos (análisis de Stinchcombe; Zavalishin, *The Art of VA Filter Design*);
  - acento y slide;
  - secuenciador de pasos.
- **Kick / 808:**
  - seno con envolvente de pitch más un cuerpo resonante;
  - click, drive y glide.
- **Sampler con SFZ v2** en `instrument.sampler`:
  - regiones, capas de velocidad, round robin y disparo en release;
  - streaming desde disco para pianos grandes, para que el Salamander Grand suene en nuestro sampler (especificación SFZ abierta; sfizz como referencia de ideas).
- **Shaper multicurva:** ampliar `fx.shaper` con curvas dibujadas de volumen, filtro y paneo, y envolventes disparadas por audio (al estilo gate12/filtr/ShaperBox).
- **Integración completa:** registrar todo en los grupos del punto 1, la bibliografía en `docs/research/12-dsp-plugin-suite.md` (Zölzer *DAFX*; Pirkle; los libros de J. O. Smith en CCRMA; Zavalishin; Välimäki; Puckette; Roads; Reiss & McPherson) y las entradas en `NOTICE` del código MIT/BSD portado (Airwindows, Chowdhury-DSP).

Cada plugin necesita:
- su tarjeta compacta y su ventana grande;
- presets de fábrica;
- tests de respuesta en frecuencia, aliasing, ausencia de NaN/denormales y cero asignaciones en `process`;
- un presupuesto de CPU por instancia.

## 5. Pendientes conocidos de lo ya entregado

- **Clip transpuesto:** en el editor de samples, la rejilla y el snap siguen los tiempos del archivo, no los compases de la canción.
- **Recorte izquierdo MIDI:** las notas ocultas al recortar el borde izquierdo se borran al soltar. Live las conserva.
- **Consolidar audio:** renderiza sin los efectos de la pista.
- **Cambiar el tono sin cambiar la velocidad:** los modos Beats, Tones, Texture y Complex de Live. Hoy solo existe Re-Pitch. Hace falta:
  - stretch granular o WSOLA para Beats y Tones;
  - vocoder de fase con bloqueo de fase para Complex;
  - preservación de formantes para Complex Pro.
- **Plugins externos:**
  - un plugin cuyo editor se cuelga dentro del proceso de la app congela la app; usar el sandbox para esos plugins;
  - las ventanas de editor del sandbox no pasan al frente solas;
  - nadie ha visto aún el aviso del watchdog dentro de la app.
- **Sin probar con hardware real:** auriculares y altavoces Bluetooth, descargas reales desde carpetas en la nube y todo lo específico de Windows.
- **Gestos de ratón** (arrastres, menús contextuales, rangos de automatización): la lógica tiene tests, pero los agentes no pudieron manejar el ratón en la app real. Hay que probarlos a mano.

## Plugins externos instalados

Están en `~/Library/Audio/Plug-Ins/{VST3,CLAP}` (por usuario, sin sudo):

- **Instrumentos:** Surge XT, Dexed, Monique, OB-Xf, JC303, sfizz, ChowKick.
- **Efectos:** Surge XT Effects, Airwindows Consolidated, CloudReverb, Dragonfly Hall/Room/Plate, ZL Equalizer 2, ZL Compressor, ZL Splitter, ChowMatrix, ChowTapeModel, ChowCentaur, BYOD, gate12, filtr, reevr, Wolf Shaper.

Retirados por inestables a `~/Library/Audio/Plug-Ins (disabled by AI-Track)/`: Dragonfly Early Reflections (su editor se cuelga), Wavetable de Socalabs (sale en silencio) y Odin 2 (se caía al descargarse).
