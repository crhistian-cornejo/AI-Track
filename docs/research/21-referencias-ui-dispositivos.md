# 21 · Referencias de interfaz para los seis dispositivos nuevos

> **Nota.** Replicamos función, flujo de trabajo y convenciones del tipo de producto; nunca la apariencia, los nombres ni la marca de un producto concreto.
> Todo lo que sigue es investigación de mercado para decisiones de diseño: `[V]` = verificado con la URL citada, `[S]` = supuesto o inferencia mía (sin fuente directa).

Fecha de la investigación: 15-nov-2026. Estado del repo leído en solo lectura (otros agentes editan a la vez; los números de línea pueden moverse unas líneas).

## 0. Método y límites

* **Verificado** quiere decir que una página oficial (manual, página de producto) o una reseña dice eso. Cuando el dato salió solo de un resumen de buscador sobre una URL que no pude abrir, lo marco `[V-resumen]`: la URL es la fuente, pero no leí la página entera.
* **No pude leer** (PDF binario, 403/404/429, reseñas truncadas): los manuales PDF de Effectrix 2, Looperator, ShaperBox 3 y Glitch 2; las reseñas de MusicRadar (cortadas tras el título); las páginas de ayuda de Pro-MB sobre dinámica; el supuesto "Stutter" de Kilohearts (404, y el índice de snapins no lo lista: **no verifiqué que exista**). Polyverse Supermodal es un filtro modal (https://www.prosoundweb.com/polyverse-launches-supermodal-1-0-0-modal-filter-plugin/): no es de stutter, lo descarto. Denise Bass XL y CrushStation: no localizados.
* No se descargó ni instaló nada (la herramienta de lectura web dejó en caché local un PDF de Sugar Bytes fuera del repo; no se usó).
* Resúmenes con mis palabras; ninguna cita larga.

---

# PARTE A · Stutter, glitch y beat-repeat

## A.1 Ficha por producto

Convención: **Gesto** = lo que el usuario hace casi todo el tiempo. **Directo** = lo que existe para tocarlo en vivo.

### Ableton Beat Repeat (referencia de base)
* **Qué hace [V]**: captura audio cada `Interval` (1/32 a 4 compases) con retardo `Offset`, lo repite durante `Gate` (en semicorcheas) con probabilidad `Chance`; `Grid` es el tamaño de cada trozo repetido (y "No Triplets" lo limita a binario); `Variation` cambia el grid al azar (Trigger, 1/4, 1/8, 1/16, Auto); `Pitch` y `Pitch Decay` bajan cada repetición; filtro paso banda con centro y anchura; modos de mezcla Mix, Insert y Gate; `Volume` y `Decay`; botón `Repeat` que ignora la temporización y repite ya hasta soltarlo. https://www.ableton.com/en/manual/live-audio-effect-reference/
* **Gesto [V]**: mover selectores/perillas y pulsar `Repeat`. No hay rejilla de pasos y el manual no describe ningún gráfico central.
* **Directo**: botón momentáneo `Repeat`, `Chance`, `Variation`; sin bancos ni MIDI (el manual no los menciona).

### Sugar Bytes Effectrix / Effectrix 2
* **Rejilla [V]**: se "pintan" bloques de colores en una rejilla de **32 pasos** (un divisor de tempo fija la duración del paso), con swing y longitud de bucle ajustable. https://sugar-bytes.de/en/effectrix y https://sugar-bytes.de/en/effectrix2
* **Efectos [V]**: Effectrix 2 tiene 14 (dos loopers A y B, Grain, Tonalizer, Crush, Filter, Phaser, Modulation, Delay, Reverb, Level, Ring, Vinyl, Spectrum); cada efecto es una fila de la rejilla. Un reseñador describe los 14 apilados en vertical sobre los 32 pasos (https://www.kvraudio.com/forum/viewtopic.php?t=612935, y https://arefyevstudio.com/en/2026/06/18/effectrix-2-review/ habla de "matriz de disparo + secuenciador de patrones").
* **Modulación por paso [V]**: rampa From-To, secuenciador de pasos con suavizado, seguidor de envolvente, LFO, una herramienta "Steptrain" para stutter y modulador aleatorio. En la v1 había **dos pistas de modulación por efecto**. https://sugar-bytes.de/en/effectrix2 y https://sugar-bytes.de/en/effectrix
* **Directo [V]**: hasta **12 patrones/ajustes por preset**, conmutables con la pantalla, un teclado MIDI o la automatización; botón "chaos", copiar/pegar, longitud de bucle, **aleatorizadores en tres intensidades** que pueden apuntar al patrón entero, a una pista de efecto o a sus parámetros; MIDI learn para bypass/chaos/random (v1.4.2). Seco/mojado por efecto y global. Más de 400 presets.

### Sugar Bytes Looperator
* **[V] (página de producto + resumen del manual)**: graba en tiempo real y corta el audio en **16 trozos** (slicer de 16 pasos); seis carriles de secuenciador que se reordenan arrastrando; cada carril ofrece efectos de una lista (el manual resumido habla de 20 listos) y 4 pasos definibles por el usuario; en el carril del slicer eliges cuál de los 16 trozos suena en cada paso; se enlazan pasos con Mayús+clic. Cada efecto tiene 5 parámetros modulables (20 envolventes, seguidor, aleatorio). Secuencias completas asignables a notas MIDI. https://sugar-bytes.de/en/looperator y https://downloads.sugar-bytes.de/manuals/Looperator.pdf `[V-resumen]`
* **Gesto**: elegir el trozo de cada paso y activar efectos por paso, en carriles apilados.

### Image-Line Gross Beat
* **Qué hace [V]**: efecto de tiempo y volumen sobre una **memoria rodante de 2 compases**; 36 ranuras de "Time" y 36 de "Volume" con envolventes; las de Time y Volume con el mismo número se pueden enlazar. https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/plugins/Gross%20Beat.htm
* **Gesto [V]**: dibujar puntos de envolvente en un área de **1 compás de ancho por 2 de alto**. En Time, lo alto es audio en directo y bajar es leer audio anterior (bajar un nodo a -2 repite los dos primeros tiempos, "Stairs" da el escalón del stutter, curvas simples dan el rascado/tape-stop). Se ajusta a una rejilla de 1/16 a 1/3 de tiempo; tipos de curva Single, Double, Hold, Stairs, Pulse, Wave, Half sine; Mayús/Ctrl bloquean ejes; hay un secuenciador de envolventes. Guía de uso: https://www.loopmasters.com/articles/4330-What-Is-Gross-Beat-And-How-To-Use-It-a-Guide
* **Pantalla [V]**: dos paneles, Time (verde) y Volume (naranja), con las ranuras como botones y el área de mapeo; perillas globales ATT (0-500 ms), REL (0-1000 ms), TNS, mezcla de volumen y de tiempo (la mezcla propia evita el comb del seco/mojado externo).
* **Directo [V]**: las ranuras se disparan por notas MIDI (opción "Trigger slots from MIDI notes"); modos *Key-held* (mientras la tecla está pulsada), *One-shot*, sincronía de disparo/posición; botón **Hold** que repite el compás y tiene tres modos (Off, Last, Next).
* **Línea de seguridad [V]**: una diagonal marca hasta dónde hay audio ya almacenado; leer por debajo en el primer compás da silencio.

### illformed Glitch 2 (antes dblue Glitch; gratuito desde ago-2026)
* **[V] https://illformed.com/glitch**: cada programa guarda **128 escenas**, cada una con su patrón, tiempos y ajustes de efectos; secuenciador multicarril donde varios efectos suenan a la vez; los bloques se **dibujan, borran, redimensionan, parten y unen** arrastrando y se pueden mover en vertical entre carriles; módulos Retrigger, Stretcher, Tape Stop, Reverser, Gater (con secuenciador de volumen) y Shuffler, cada uno con tiempo sincronizado (semicorcheas, tresillos), filtro, mezcla, pan y volumen; **las escenas se disparan por nota MIDI** (cada tecla una escena) en modo *gate* o *latch*, con un **bloqueo** para editar sin que entren cambios; **Randomize** reconstruye la escena y **Mutate** hace variaciones pequeñas de un efecto.
* Gratis: https://synthtopia.com/content/2026/08/04/illformed-glitch-2-now-free-in-memory-of-developer-kieran-foster

### Cableguys ShaperBox 3: TimeShaper, VolumeShaper, PanShaper (y Kickstart 2)
* **TimeShaper [V] https://www.cableguys.com/timeshaper**: se dibuja una forma tipo LFO que gobierna la **posición del cabezal de lectura**: patrón repetido = stutter, hacia atrás = reverse, deceleración = tape-stop. Bucles de 1/128 a 32 compases o en Hz/ms; **9 ranuras de ondas propias**; voltear/redimensionar/mover/aleatorizar una onda o una selección; copiar y pegar entre shapers; sesgado de la selección (intensidad creciente); botón 2x/3x; deshacer ilimitado; **tres bandas con LFO propio**; disparo por MIDI o por audio; osciloscopio, espectro y una pantalla de desplazamiento temporal.
* **VolumeShaper [V] https://www.cableguys.com/volumeshaper**: lo mismo para volumen (ducking, gate, ritmos): curvas, escalones o rampas dibujados; el LFO puede **dispararse por MIDI, por entrada de sidechain externa (un bombo de otra pista) o por transitorios de audio** que reinician el LFO; suavizado del disparo y pre-suavizado con anticipación; las ondas propias se cambian por MIDI. Detalle del disparo por audio (botón Audio, luego External Sidechain, ajuste de detección en el engranaje): https://www.cableguys.com/volumeshaper-4-manual
* **PanShaper [V] https://www.cableguys.com/panshaper**: LFO dibujado para el pan; mismas herramientas; por bandas; panning de balance (0, 3, 4,5 o 6 dB) o de Haas (0-40 ms) y mezcla entre ambos; seguidores de envolvente; vectorscopio, espectro y osciloscopio con modo magnitud y congelado.
* **Conjunto [V] https://www.cableguys.com/shaperbox**: 11 shapers con una sola interfaz; incluye CrushShaper (sin detalle en la página).
* **Kickstart 2 [V] https://www.kickstart-plugin.com/**: ducking por golpe de bombo; **16 curvas** de fábrica; se **arrastra la curva** para adaptarla a bombos cortos o largos; un mando "Big Mix"; sigue patrones que no son 4/4 con disparo por audio; modo multibanda (solo duckea los graves); vista del bombo y el bajo juntos.

### Xfer LFOTool `[V-resumen]`
* https://www.xferrecords.com/products/lfo-tool (y https://www.kvraudio.com/news/xfer-records-updates-lfotool-to-v1-1-win-and-mac-vst-and-au-19838): editor de gráficos de **puntos + tensión**; **12 gráficos por preset** conmutables por notas MIDI o automatización; hasta **4 gráficos a la vez** (volumen, corte, resonancia, pan…); BPM con swing o Hz; modo envolvente (una sola pasada tras la nota); osciloscopio sincronizado; salida de CC MIDI.

### Kilohearts (Trance Gate, Gate, Bitcrush)
* **Trance Gate [V] https://kilohearts.com/products/trance_gate**: clic en un paso lo activa; arrastrar **une pasos** (ligadura); longitud de patrón variable; **8 ranuras** de patrón; ADSR de la compuerta; Resolution (duración del paso); Mix.
* No hay "Stutter" verificado en https://kilohearts.com/docs/snapins. Snap Heap es un contenedor de efectos en carriles (https://kilohearts.com/docs/snap_heap).

### Sonic Charge Permut8 `[V-resumen]`
* https://www.pluginboutique.com/articles/137-Macprovideo-review-Sonic-Charge-Permut8 : retardo digital de 12 bits con reloj variable 0-352 kHz (a 0 Hz es un paro de cinta mezclado con reducción de tasa); **dos módulos de 16 interruptores** sincronizados al tempo (disparables por MIDI o automatización); en modo Freq las notas MIDI cambian la velocidad de reproducción, en modo Prog cambian de programa. El stutter sale de bancos de firmware, no del plugin base.

### Devious Machines Infiltrator 2 `[V-resumen]`
* https://synthanatomy.com/2022/09/devious-machines-infiltrator-2-free-update-unlocks-a-sequenced-multi-effects-evolution.html : **secuenciador de 32 pasos** que activa o no cada uno de los 10 módulos por paso; cada efecto lleva **dos envolventes multisegmento sincronizadas** con editor gráfico, 8 formas de fábrica, aleatorizador, swing y ajuste a nota; 4 macros; disparo manual, por MIDI o por el secuenciador; salida de CC MIDI.

### Output Movement `[V-resumen]`
* https://splice.com/blog/output-movement-feature-guide/ : "ritmos" (secuenciador de pasos, LFO, sidechain) que se asignan a parámetros de efectos del propio plugin (filtro, pan, volumen…); el editor del ritmo se abre al pulsar su icono; **Flux** modula la velocidad del ritmo. Las fuentes discrepan en el recuento de motores.

### iZotope Stutter Edit 2 (no estaba en la lista, pero es el referente del género)
* **[V] https://www.izotope.com/en/learn/learn-stutter-edit-in-10-minutes**: efecto "tocable": cada *Gesture* (cadena de efectos sincronizada) está asignada a una nota MIDI, agrupada en *Banks*; **Auto Mode** lo hace funcionar como insert normal. Panel de gesto: Length (de 1/16T a 2 compases), **Release** (On Grid, Full Gesture, Latch, Instant, Stick), Grid, **Freeze** (captura un trozo y lo repite), Palindrome. Módulo Stutter: Rate y Range, **Quantize** con Step y Mode (Free, Closest, Walk, Skip, Stagger, Random), Gate con Width y Tail. Módulo Buffer: posición L/R enlazable, Reverse, Movement (Slider, Grid, Random), Jitter, Pan/Width. 11 procesadores de efecto cuyos parámetros se asignan a **curvas en el tiempo** (editor de curvas).

### Dblue/otros no localizados con detalle
* "Kilohearts Stutter", "Polyverse Supermodal" (filtro modal), "Denise Bass XL" y "dBlue CrushStation": sin datos útiles (ver §0).

## A.2 Respuestas a las cinco preguntas

### 1. Gesto principal de edición, pasos y qué se edita por paso
| Producto | Gesto principal | Pasos / resolución | Qué se edita por paso | Fuente |
|---|---|---|---|---|
| Beat Repeat | Perillas/selectores + botón Repeat | No hay rejilla | — (Interval/Grid/Gate globales) | manual de Live |
| Effectrix 2 | **Pintar bloques** en rejilla, una fila por efecto | 32 (longitud ajustable, swing) | efecto activo + modulación del paso (rampa, secuenciador, LFO…) | sugar-bytes.de |
| Looperator | Elegir trozo y efecto por paso, carriles apilados | 16 trozos | qué trozo suena, parámetros de efecto, ligaduras | sugar-bytes.de, manual `[V-resumen]` |
| Gross Beat | **Dibujar nodos** de envolvente (tiempo y volumen) | área 1 compás × 2, snap 1/16-1/3 de tiempo | posición de lectura / volumen | manual de FL |
| Glitch 2 | **Dibujar bloques** en carriles | escenas de 128 | módulo, longitud, filtro/mezcla/pan/volumen; Gater con secuenciador de volumen | illformed.com |
| TimeShaper/VolumeShaper/PanShaper | **Dibujar la onda** (LFO) | bucle 1/128-32 compases | altura de la curva en cada instante | cableguys.com |
| LFOTool | Puntos + tensión | 12 gráficos/preset, 4 a la vez | altura de curva | xferrecords.com `[V-resumen]` |
| Kickstart 2 | Elegir 1 de 16 curvas y arrastrarla | — | forma | kickstart-plugin.com |
| Trance Gate | Clic y arrastre para unir | longitud variable, 8 patrones | on/off/ligado | kilohearts.com |
| Permut8 | Interruptores | 16 × 2 módulos | on/off | pluginboutique `[V-resumen]` |
| Infiltrator 2 | Rejilla de pasos × módulos + 2 envolventes por efecto | 32 | qué efectos están activos (cada paso con su longitud) | synthanatomy `[V-resumen]` |
| Stutter Edit 2 | **Tocar teclas** que disparan gestos con curvas | — | curva por parámetro | izotope.com |

Conclusión `[S]` sobre el gesto: la mayoría de los "secuenciadores de efectos" usan **una rejilla de 16-32 pasos donde el clic o el arrastre horizontal activa/liga celdas**, y los "formadores de tiempo" usan **una curva dibujada**. La probabilidad por paso solo aparece verificada en Beat Repeat (`Chance`) y Stutter Edit (modo Random); en las rejillas verificadas (Effectrix, Trance Gate, Infiltrator, Permut8) el paso es on/off sin barra de probabilidad en las fuentes que leí.

### 2. Organización de pantalla
* **Zona grande de edición arriba y controles debajo**: ShaperBox (editor del LFO grande + selector de bandas + fila de perillas) `[V]`, Gross Beat (área de mapeo con paneles Time/Volume) `[V]`.
* **Varios carriles a la vez, apilados**: Effectrix (una fila por efecto), Looperator (seis carriles reordenables), Glitch 2 (carriles de bloques) `[V]`.
* **Selector de variaciones como fila de botones numerados**: Gross Beat (36+36 ranuras), Trance Gate (8 patrones), LFOTool (12 gráficos), Effectrix (12 secuencias) `[V]`.
* **Módulos con paneles separados**: Stutter Edit 2 (Gesture / Stutter / Buffer / Effects) `[V]`.
* Disposición exacta de LFOTool, Infiltrator y Movement: `[S]` más allá de lo resumido arriba.

### 3. Directo (live)
* **Bancos de patrones**: 12 (Effectrix), 128 escenas (Glitch 2), 36+36 (Gross Beat), 12 (LFOTool), 8 (Trance Gate) `[V]`.
* **Lanzamiento por nota MIDI**: Glitch 2 (gate o latch + bloqueo de edición), Effectrix, Looperator, Gross Beat, Stutter Edit 2, LFOTool, VolumeShaper/TimeShaper `[V]`.
* **Latch/hold/freeze**: Gross Beat Hold (Off/Last/Next), Stutter Edit Release (Latch, Stick…) y Freeze, Beat Repeat `Repeat` momentáneo `[V]`.
* **Aleatorizar/variaciones**: Effectrix (3 intensidades y por alcance), Glitch 2 (Randomize/Mutate), Looperator (generador "smart"), Beat Repeat `Variation`, ShaperBox (aleatorizar selección) `[V]`.
* **A/B**: ningún stutter lo ofrece en lo que leí; sí Pro-MB (barra superior). `[V]` para Pro-MB (https://www.fabfilter.com/help/pro-mb/using/overview), `[S]` ausencia en stutters.

### 4. Funciones imprescindibles que podríamos no tener
Tabla de brechas frente a `fx.stutter` (detalle en la Parte C):
| Función del mercado | Quién | Nosotros |
|---|---|---|
| Varios carriles visibles a la vez (div, pitch, reverse…) | Effectrix, Looperator, Glitch 2 | **No** (solo el paso seleccionado en una tarjeta) |
| Volumen/compuerta por paso | Glitch 2 (Gater), Gross Beat (panel Volume), Trance Gate | **No** (solo `Decay` global) |
| Tape-stop / reverse por paso | Glitch 2 (módulos), Effectrix | Parcial: reverse por paso sí (`STEP_REV`); tape-stop solo global (`TAPE_STOP`) |
| Pitch por paso | Stutter Edit, Looperator | Sí (`STEP_PITCH`) |
| Filtro por paso o con rampa | Effectrix, Glitch 2 (cada módulo), Stutter Edit | Parcial: filtro global (`FILTER_*`), sin rampa ni por paso |
| Swing | Effectrix, Infiltrator, LFOTool | **No** |
| Bancos/escenas y recall por MIDI | todos los anteriores | **No** |
| Disparo por audio (transitorio/sidechain) | VolumeShaper, TimeShaper, Kickstart | **No** (`has_sidechain: false`, `stutter.rs:804`) |
| Ducking al bombo | Kickstart, VolumeShaper | Parcial: modo `Duck` cierra el seco durante el evento; no sigue al bombo |
| Rampas From-To en el evento | Effectrix 2, Stutter Edit (curvas) | Parcial: `RAMP` acorta el grid; sin rampa de pitch/filtro/nivel |
| Congelar/bloquear el buffer | Stutter Edit Freeze, Gross Beat Hold, Beat Repeat | Sí (`HOLD`, `stutter.rs:898`), pero solo conmutador |
| Aleatorizar/mutar | Glitch 2, Effectrix | **No** en el editor (existe `SEED`, no se muestra; `randomize_macros` en `editing/racks.rs:298` es solo de macros) |
| Sincronía con la rejilla del proyecto | todos | Sí |
| Longitud de patrón ajustable arrastrando | Effectrix, Trance Gate, ShaperBox | Parcial: `LENGTH` es un selector, no un asa |

### 5. Flujos que todos repiten y el usuario espera
1. **Escuchar al instante** lo que se dibuja: el patrón suena en bucle mientras se edita (Effectrix, Gross Beat con su memoria de 2 compases) `[S]` — no hay frase de manual que lo diga, es la forma de uso inherente.
2. **Longitud del patrón** distinta de la del compás y rejilla con múltiples (Effectrix "loop length", Trance Gate, ShaperBox 1/128-32 compases) `[V]`.
3. **Copiar/pegar y duplicar** (Effectrix copy/paste, ShaperBox 2x/3x y copia entre shapers) `[V]`.
4. **Aleatorizar y mutar** con intensidad y alcance (Effectrix, Glitch 2) `[V]`.
5. **Probabilidad** (Beat Repeat `Chance`, Stutter Edit Random) `[V]`.
6. **Bloquear/congelar** el buffer y soltar al tocar (Hold/Freeze/Repeat) `[V]`.
7. **Modos de mezcla**: Mix, Insert, Gate (Beat Repeat) `[V]`; los nuestros son Insert/Mix/Duck/Gate (`stutter.rs:1-40` doc).
8. **Anti-clic**: opción de reducción de clics (Gross Beat), suavizado de disparo (VolumeShaper) `[V]`; nosotros ya hacemos fundidos de 1,5-2 ms (doc del módulo).
9. **Presets en categorías** (Effectrix 400+, Looperator 300+) `[V]`; nosotros: 21 en `crates/stock/presets/fx.stutter/` (Build-Up, Glitch, Half-Time, Stutter, Tape, Vocal).


---

# PARTE B · Convenciones de las otras cinco categorías

Para cada una: gráfico central, qué se arrastra sobre él, qué se mueve mientras suena, qué se considera obligatorio.

## B.1 Pan automático / trémolo
Referencias: Auto Pan-Tremolo de Live, Cableguys PanShaper, Melda MAutoPan, Soundtoys PanMan. (TAL-Chorus-LX: la búsqueda no devolvió nada; **sin verificar**.)
* **Gráfico central [V]**: la forma del LFO. En Live hay una visualización en tiempo real bajo la forma que muestra la posición estéreo (Pan) o la amplitud (Tremolo) (https://www.ableton.com/en/manual/live-audio-effect-reference/); PanShaper dibuja el LFO a pantalla grande y añade vectorscopio, espectro y osciloscopio (https://www.cableguys.com/panshaper).
* **Arrastrable**: en la familia ShaperBox la **onda misma** (dibujar, voltear, redimensionar, mover, sesgar, 2x/3x, 9 ranuras propias) `[V]`; en Live no se describe nada arrastrable `[V]` (solo perillas). MAutoPan: LFO dibujable `[V-resumen]` (https://www.toolfarm.com/?p=11202). PanMan: formas personalizadas, patrones de pasos y aleatorios; "trigger divider" que cuenta disparos antes de cambiar de posición `[V-resumen]` (https://soundtoys.com/?p=122).
* **Animado**: cabezal sobre la onda y marcador de posición/ganancia `[V]` (Live); vectorscopio `[V]` (PanShaper).
* **Obligatorio**: forma, velocidad libre y sincronizada (con puntillo y tresillo), amount/profundidad, desfase estéreo (Phase/Spin en Live), invert, mezcla; **añadidos de mercado**: disparo por MIDI o audio (PanShaper), pan por balance (0/3/4,5/6 dB) o por Haas (0-40 ms) (PanShaper), seguidor de envolvente, modo trémolo (Live: Harmonic y Vintage) `[V]`.

## B.2 Reductor de bits / downsampler
Referencias: Redux de Live, D16 Decimort 2, TAL-Bitcrusher, Tritik Krush, Kilohearts Bitcrush.
* **Gráfico central**: el manual de Live **no describe ningún gráfico** `[V]`. Decimort tiene "una interfaz muy mejorada" y es el que más control expone (https://d16.pl/decimort2, https://www.pluginboutique.com/product/2-Effects/32-Bit-Crusher/2017-Decimort-2 `[V-resumen]`). `[S]` el gráfico "obligatorio" que usa el género es una forma de onda escalonada o una curva de transferencia; no encontré captura descrita.
* **Controles que sí se consideran básicos [V]**: Rate (Redux, Kilohearts con mínimo de 200 Hz), Bits, Jitter, filtro previo y posterior (Redux; Decimort con dos), Shape de cuantizador (Redux), DC Shift (Redux), **Dither** (Decimort, Kilohearts), calidad de convertidores ADC/DAC (Kilohearts), mid-tread/mid-riser (Decimort), ruido (TAL), drive y filtro resonante con LFO (Krush) y mezcla.
* **Arrastrable/animado**: no hay fuente `[S]`.

## B.3 Compresor de bus "glue" VCA
Referencias: The Glue (Cytomic), SSL G-Bus de Waves, Kotelnikov, MJUC, Glue Compressor de Live.
* **Gráfico central**: en Live, una **aguja de reducción de ganancia en dB** y un LED de clip (rojo >0 dB, amarillo cuando actúa el Soft Clip) `[V]` (manual de Live). En Waves, un medidor de reducción en dBu `[V-resumen]` (https://www.waves.com/how-to-use-ssl-bus-compressor-classic-mix-glue). No hay curva de transferencia arrastrable en estos productos `[S]`; es una convención de otros compresores de pantalla grande.
* **Obligatorio [V]**: Threshold, Ratio (2/4/10), Attack (0,01-30 ms), Release (0,1-1,2 s + Auto), Makeup, Mix, Range (Live), Soft Clip (Live), filtro de sidechain (HPF) y escucha del sidechain, sidechain externo con EQ (Live), Autofade (Waves). Kotelnikov añade release independiente para picos y RMS, ratio dependiente de la frecuencia y HPF de pendiente variable (https://docs.tokyodawn.net/kotelnikov-manual/ `[V-resumen]`). MJUC añade modos Mk1/2/3, Timbre, Drive, mezcla y auto-makeup (https://klanghelm.com/contents/products/MJUC `[V-resumen]`).
* Cytomic The Glue: Fade y Peak Clip **no verificados** (los resúmenes solo confirman umbral, makeup, 2/4/10, ataque y release, Mix, Range, HPF y oversampling).

## B.4 Procesador de bus de batería
Referencias: Drum Buss de Live, Waves Smack Attack, Sonnox Drum Gate, Newfangled Saturate.
* **Drum Buss [V]** (manual de Live): Trim, Comp (conmutador), tipo de distorsión Soft/Medium/Hard, Drive, Crunch, Damp, Transients (+/- por encima de 100 Hz), Boom (cantidad), Freq con **Force To Note**, Decay, **Boom Audition** (solo del grave), Output y Dry/Wet; "Bass Meter" que muestra el efecto del Boom. No describe un gráfico arrastrable.
* **Smack Attack [V-resumen]** (https://www.fullcompass.com/common/files/29478-WavesSmackAttackUserGuide.pdf): ataque y sustain con Level y Sensitivity propios, Shape, Duration, Wet/Dry y sección **Guard** (Off, Clip o Limit a -0,1 dBFS).
* **Drum Gate** (Sonnox): rango de la compuerta, filtro de sidechain, ajustes de transitorio; **Drum Gate 2** añade entrada externa y **MIDI** (https://www.kvraudio.com/news/sonnox-releases-oxford-drum-gate-2-66172 `[V-resumen]`).
* **Saturate [V-resumen]** (https://www.newfangledaudio.com/saturate): Shape (duro a suave), Symmetry (armónicos pares), Ceiling, **escucha de la diferencia (Delta)**, medidores pico y RMS de entrada y salida.
* **Convención**: pocos mandos por etapa + un "solo" de la parte añadida (Boom Audition, Delta) para oír qué hace cada etapa. `[S]` como generalización.

## B.5 Dinámica multibanda ascendente y descendente
Referencias: OTT, Pro-MB, Multiband Dynamics de Live, MMultiBandDynamics, Neutron.
* **Gráfico central [V]**:
  * Live: **bloques de cada banda** con barras grandes de nivel de salida y pequeñas de entrada y una escala en dB (https://www.ableton.com/en/manual/live-audio-effect-reference/).
  * Pro-MB: pantalla de frecuencia donde cada banda es un rectángulo con un punto de pico; una curva amarilla gruesa muestra la respuesta dinámica instantánea; analizador de espectro opcional (https://www.fabfilter.com/help/pro-mb/using/display).
  * OTT: tres bandas con entrada y reducción; umbrales de compresor y expansor en tonos distintos `[V-resumen]` (https://www.kvraudio.com/news/xfer-records-releases-ott-multiband-compressor-for-windows-and-os-x-vst-and-au-19972).
  * MMultiBandDynamics: curva de procesamiento editable con el nivel real de la fuente dentro del gráfico `[V-resumen]` (https://www.meldaproduction.com/MDynamicsMB).
* **Arrastrable [V]**: Live: **bordes de bloque = umbrales** (el cursor pasa a corchete), **centro del bloque = nivel** de ese rango; con Cmd/Ctrl se aplica a todas las bandas, con Alt se mueven juntos los lados Above y Below, con Mayús es fino, con doble clic se restablece. Pro-MB: punto de pico (ganancia/frecuencia), crossovers sueltos, pendiente de crossover (6-48 dB/oct), línea de **Range** (arrastrar arriba/abajo cambia el parámetro); doble clic escribe valores; zoom en la escala de frecuencias.
* **Animado**: barras de entrada/salida, curva de ganancia en tiempo real, anillo del medidor alrededor de Threshold (Pro-MB muestra la señal de detección alrededor del mando) `[V]` (https://www.fabfilter.com/help/pro-mb/using/basicbandcontrols).
* **Obligatorio [V]**: crossovers, por banda umbral/ratio, ataque/release, ganancia de banda, solo/mute o activador; globales Time, Amount (Live) o Depth, Input/Output, Peak/RMS (Live), Soft Knee (Live), sidechain. Pro-MB añade Lookahead (hasta 20 ms), Stereo Link/M-S y modo Compress/Expand con signo del Range (cuatro tipos de proceso). OTT: In, Out, Time, Depth y, por banda, Upward y Downward.
* Neutron: vista de espectro de crossover con "Learn" y compresión ascendente y descendente `[V-resumen]` (https://s3.amazonaws.com/izotopedownloads/docs/neutron202/en/compressor/index.html).


---

# PARTE C · Estado actual y rediseño, dispositivo a dispositivo

Reglas que condicionan todos los cambios (CLAUDE.md y memoria del proyecto): ids de parámetro estables (nunca renumerar; los nuevos van con ids nuevos y los presets existentes toman el defecto), `ParamInfo::choices` con longitud igual al rango, `meter()` ≤ 320 valores, nada de colores sueltos (`theme.rs`), espaciado en múltiplos de 4 px, toda edición es un `Command`. Las primitivas de editor que ya existen y conviene reutilizar están en `crates/app/src/views/device_ui/kit.rs`: `kit::plot` con `PlotCfg { handles, on_empty, on_context, keys }`, `Handle { fx, fy, x, y, alt, index, reset }` (el campo `alt` es "Alt + arrastre vertical y rueda", `kit.rs:219`; `on_empty` es doble clic en vacío; `on_context` clic derecho en asa), `kit::ctl/num/pick/section`.

Esfuerzo: **S** = menos de media jornada, **M** = 1-2 jornadas, **L** = varios días o toca el motor/modelo.

## C.0 Brechas entre el doc 18 y lo implementado
Lo prometido en `docs/research/18-dispositivos-nativos-racks.md` y que el editor aún no hace (verificado leyendo ambos):
* C.1 autopan: "arrastrar sobre el visor cambia Shape (vertical) y Phase (horizontal)" y "barra de campo estéreo/ganancia": **no**; solo el asa de Amount (`autopan.rs`(ui):125).
* C.2 crusher: "asa horizontal que mueve DC Shift", barra del Nyquist: **no** (solo asas Bits y Rate, `crusher.rs`(ui):83-84).
* C.3 buscomp: "panel de sidechain plegable con curva de HPF": **parcial** (selector de HPF sin curva).
* C.4 mbdyn: "espectro de fondo" y "Alt+arrastre = ratio": **no** (`Handle.alt` sin usar en `mbdyn.rs`(ui):283-307).
* C.5 stutter: "curva de pitch superpuesta" sobre la forma de onda y "botón grande HOLD momentáneo": **no**; `HOLD` es un `kit::pick` conmutador (`stutter.rs`(ui):179).
* C.7 drumbus: "ajuste a la nota/tonalidad" para Boom Freq y "medidor de reducción en cada mini-panel": **parcial**.

(En las citas, `x.rs`(ui) es `crates/app/src/views/device_ui/x.rs` y `x.rs` a secas es `crates/stock/src/fx/x.rs`.)

---

## C.5 `fx.stutter` (el más lejano del mercado; va primero)

### (1) Tabla comparativa
| Control / función del mercado | ¿Lo tenemos? | Dónde |
|---|---|---|
| Interval 1/32-4 compases, Offset, Chance, Gate, Grid, No-Triplets, Variation (Beat Repeat) | **Sí** | `p::INTERVAL, OFFSET, CHANCE, GATE, GRID, TRIPLETS, VARIATION` (`stutter.rs`:67-90) |
| Pitch y Pitch Decay | Sí | `p::PITCH`, `p::PITCH_DECAY` |
| Filtro paso banda (centro/anchura) | Sí | `p::FILTER_ON, FILTER_FREQ, FILTER_WIDTH` |
| Modos Mix/Insert/Gate | Sí (+ Duck) | `p::MIX_MODE` (`MIX_MODES`: Insert, Mix, Duck, Gate) |
| Volume/Decay | Parcial | `p::DECAY` (dB por repetición); sin volumen por paso |
| Botón Repeat momentáneo / Hold / Freeze | Parcial | `p::HOLD` existe; en el editor es conmutador |
| Rejilla de pasos (16-32) | Sí (16) | `STEPS = 16`, `p::LENGTH` |
| On/Off por paso | Sí | `p::STEP_ON` |
| Probabilidad por paso | Sí (más que el mercado verificado) | `p::STEP_PROB` |
| División (tamaño de trozo) por paso | Sí | `p::STEP_DIV` (1/8…1/32T) |
| Pitch por paso | Sí | `p::STEP_PITCH` ±12 |
| Reverse por paso | Sí | `p::STEP_REV` (Fwd/Rev) |
| Nivel/compuerta por paso (Gater, panel Volume) | **No** | — |
| Tape-stop por paso | **No** (global) | `p::TAPE_STOP`, `p::STOP_TIME` |
| Filtro por paso | **No** | — |
| Ligadura de pasos (Trance Gate, Looperator) | **No** | — |
| Swing | **No** | — |
| Longitud de bucle en pasos | Sí (selector) | `p::LENGTH` |
| Rampa de aceleración (1/4→1/32) | Sí | `p::RAMP` (x2…x16) |
| Rampas From-To de pitch/filtro/nivel dentro del evento | **No** | — |
| Half-time | Sí | `p::HALF_TIME` |
| Aleatorizar / mutar | **No** en la UI (hay `SEED`, no visible) | `p::SEED` |
| Escenas/bancos de patrón | **No** | — |
| Disparo por nota MIDI | **No** | — |
| Disparo por transitorio / sidechain externo | **No** | `has_sidechain: false`, `stutter.rs`:804 |
| Varios carriles visibles a la vez | **No** | solo paso seleccionado: `stutter.rs`(ui):167,186 |
| Forma de onda del trozo con posición de lectura | Sí | `stutter.rs`(ui):145-160, meter 128 picos |
| Presets | 21 | `crates/stock/presets/fx.stutter/` |

### (2) Cambios priorizados
| # | Cambio | Esfuerzo | Toca DSP |
|---|---|---|---|
| 1 | **Pintar arrastrando** sobre la rejilla (el primer paso fija si se activa o se apaga y el resto lo copia), probabilidad en carril propio, no por altura de la celda | S | no |
| 2 | **Carriles apilados** siempre visibles: Probabilidad, División, Pitch, Reverse (y después Nivel, Stop), con las mismas 16 columnas, en vez de la tarjeta del paso seleccionado | M | no |
| 3 | Asa arrastrable de **longitud** (borde derecho de la rejilla) y botones Copiar/Pegar patrón, Rotar ←/→, ×2 (duplica la primera mitad en la segunda), Borrar | S | no |
| 4 | **Randomize / Mutate** como `Command`s (rellena `STEP_*` con semilla; Mutate cambia 2-3 pasos), con deshacer | S | no |
| 5 | **HOLD como pad**: mantener = momentáneo (como `Repeat` de Live); Mayús+clic = latch; atajo de teclado en `keys.rs` TABLE (memoria: "Keys table mirrors actions.rs") | S | no |
| 6 | **Nivel por paso** (`STEP_LEVEL` = campo 5 del paso) y **Stop por paso** (`STEP_STOP` = campo 6) y, si cabe, **Filtro por paso** (`STEP_FILTER` = campo 7): `STEP_STRIDE = 8` ya deja libres los campos 5-7, sin renumerar nada | S-M | **sí** |
| 7 | **Swing** (`p::SWING` id nuevo ≥ 26): retrasar las fronteras de paso impares, 0-75 % | S-M | **sí** |
| 8 | **Ligadura de pasos** (un paso sostiene al anterior: evento de longitud doble) | M | **sí** |
| 9 | **Rampas From-To** de pitch, filtro y nivel a lo largo del evento (`PITCH_END`, `FILTER_END`, `LEVEL_END`) y superponer la curva sobre la forma de onda | M | **sí** |
| 10 | **Escenas A-D**: cuatro patrones completos conmutables por automatización, por nota MIDI o por botones; opción económica = instantáneas de parámetros hechas en el modelo con `Command` (sin tocar el DSP), opción robusta = param `p::SCENE` y cuatro copias de los pasos con ids ≥ 1000 | L | según la opción |
| 11 | **Disparo externo**: sidechain (transitorio) y/o nota MIDI como origen del evento en lugar del reloj (Source: Clock / Hold / Audio key / MIDI note) | L | **sí**; el motor ya tiene marco de efectos MIDI (`graph.rs`:77) pero **no verifiqué** que un efecto de audio reciba `NoteOn` de su pista |
| 12 | Panel de **ducking al bombo** (Kickstart/VolumeShaper) como modo `Duck` con disparo externo | M | **sí** (depende de 11) |
| 13 | Mostrar `Seed` y poner un botón de dado | S | no |

### (3) Diseño propuesto (pantalla)
Ancho 520 (compacto) o 640 (`dev.big`), mismo `kit::panel`.

1. **Cabecera** (una fila, 28 px): `Mode` (Classic/Grid), cuatro chips `A B C D` de escena (inactivos hasta que exista el cambio 10; mientras tanto se ocultan), **pad HOLD** (botón 56×28 px: mantener = hold momentáneo, Mayús+clic = latch, el color de acento mientras está activo), dado "Randomize" y "Mutate" (iconos de `icons.rs`), a la derecha `Mix Mode`, `Mix`, `Out`.
2. **Rejilla** (solo en modo Grid): 16 columnas iguales con separación de 4 px y guía más marcada cada 4 pasos (ya existe el tono alterno `i/4 % 2`). Debajo del número de paso, carriles de 24-40 px, etiquetados a la izquierda con un rótulo de 8 px y un botón de plegado:
   * **Carril GATE** (40 px): celda activa en color de acento; apagada, tenue. Clic = alternar. **Arrastre horizontal** = pintar el estado del primer paso tocado sobre todos los pasos recorridos. **Arrastre vertical con Alt** (o Mayús) = probabilidad; el relleno de la celda muestra la probabilidad (altura) y el texto `%` aparece al pasar el cursor.
   * **Carril DIV** (24 px): chip por paso con `1/8 · 1/16 · 1/16T · 1/32 · 1/32T`; clic cicla, rueda cambia, arrastre vertical cambia.
   * **Carril PITCH** (36 px): barras bipolares ±12 con línea central; ajuste a semitono; doble clic = 0.
   * **Carril LEVEL** (28 px, nuevo): barra de 0 a -inf (compuerta de volumen por paso).
   * **Carril FLAGS** (20 px): dos puntos por paso, `REV` y `STOP` (tape-stop del paso).
   * El paso que suena (de `meter()[2]`) resalta su columna entera; en la base de la columna activa, una barra de progreso del evento (`meter()[4]`).
   * **Asa de longitud**: separador vertical a la derecha del último paso activo, arrastrable (cambia `p::LENGTH`); pasos fuera de la longitud al 50 % de opacidad (ya ocurre).
   * Clic derecho en una celda (`on_context`): Copiar paso, Pegar, Borrar, Rellenar cada 2 / cada 4.
3. **Modo Classic**: se mantiene la línea de tiempo con el evento dentro del intervalo (como hoy) más el contador de `Variation`; el mismo pad HOLD.
4. **Visor del trozo** (48 → 72 px): forma de onda del trozo congelado, marca de bucle, cabezal de lectura, y **superposición de la curva de velocidad `r`** (`meter()[3]`) como línea fina; con tape-stop, la caída exponencial se ve en la propia curva. Es la pieza que da retroalimentación inmediata.
5. **Tarjetas** (abajo, plegables): REPEAT (Step Rate, Steps), TONE (Pitch, Pitch Decay, Decay, Reverse, Ramp, y From→To cuando exista), TAPE-STOP, HALF-TIME, FILTER, y una nueva **TRIGGER** (Source: Clock / Audio / MIDI; Sensibilidad; Hold-mode) cuando exista el cambio 11.

Presets nuevos sugeridos al añadir los campos: "Stutter house 3-3-2 con swing", "Roll de caja con rampa de nivel", "Gate rítmico con Stop al final de compás".

`[S]` Valoración del flujo de stutter house: lo que más repite el género es un ciclo de 1/16 con probabilidad, un roll acelerado de subida, un tape-stop al final de frase, reverse puntual y corte rítmico de volumen; todo eso ya lo cubre el motor menos el **corte de volumen por paso**, el **swing** y el **disparo en vivo** (hold + MIDI). No encontré fuente que cuantifique el uso real por género.

---

## C.1 `fx.autopan`

### (1) Tabla comparativa
| Control / función del mercado | ¿Lo tenemos? | Dónde |
|---|---|---|
| Modo Pan / Tremolo (Live) | Sí | `p::MODE` (`autopan.rs`:33) |
| 9 formas de onda (Live) | Sí | `p::WAVE` (`Waveform::NAMES`) |
| Hz / tiempo / sincronizado con puntillo y tresillo | Sí | `p::TIME_MODE, RATE, TIME, DIVISION` |
| Phase y Spin (Live) | Sí | `p::PHASE, STEREO_MODE, STEREO` |
| Invert | Sí | `p::INVERT` |
| Amount, Shape, Attack, Dynamic FM | Sí | `p::AMOUNT, SHAPE, ATTACK, DYN_FM` |
| Harmonic y Vintage (tremolo) | Sí | `p::HARMONIC, VINTAGE` |
| Mezcla y salida | Sí | `p::MIX, OUTPUT` |
| Visualización en tiempo real de posición/ganancia (Live) | Parcial | puntos vivos sobre las curvas (ui:151-160), sin barra de campo estéreo |
| **Onda dibujada por el usuario** (PanShaper, MAutoPan, PanMan) | **No** | — |
| Ranuras de ondas propias (9 en ShaperBox) | **No** | — |
| Disparo por MIDI / transitorio (reinicio de fase) | **No** | — |
| Pan por Haas (0-40 ms) y elección de ley de pan | **No** | ley de potencia constante fija |
| Por bandas (3 bandas con LFO propio) | **No** | solo `Harmonic` (corte 600 Hz) |
| Seguidor de envolvente como fuente | Parcial | `Dynamic FM` modula la velocidad, no la profundidad |
| Vectorscopio / osciloscopio | **No** | — |
| Arrastrar sobre la onda (Shape/Phase/Stereo) | **No** (prometido en el doc 18) | ui:125 solo Amount |
| 15 presets | Sí | `crates/stock/presets/fx.autopan/` |

### (2) Cambios priorizados
| # | Cambio | Esfuerzo | DSP |
|---|---|---|---|
| 1 | Manipulación directa sobre el visor: arrastre horizontal de la curva L = `Phase`; asa en el pico de R = `Stereo`; asa vertical en el pico = `Shape`; rueda = `Rate`/`Division` (con `Handle.x/y/alt`); doble clic = defecto | S | no |
| 2 | Barra inferior animada: en Pan, banda de campo estéreo -1…+1 con marcador (de `meter()` L/R); en Tremolo, barra de ganancia | S | no |
| 3 | Selector de forma como chips con icono de onda en vez de un desplegable (como en doc 18) | S | no |
| 4 | **Reinicio de fase** por nota MIDI o transitorio (`RETRIGGER`: Off / Transport / Audio) con suavizado | M | **sí** |
| 5 | **Onda dibujada** de 16 o 32 puntos con interpolación (`Waveform::Custom`) y 4 ranuras guardadas con el proyecto; edición: clic-arrastre sobre el visor pinta, Mayús+arrastre traza una línea, 2x/3x, voltear | L | **sí** (parámetros o bloque de estado vía `save_state`) |
| 6 | Modo **Haas** (retardo 0-40 ms) y mezcla balance↔Haas; selector de ley de pan (0/3/4,5/6 dB) | M | **sí** |
| 7 | Suavizado ajustable (`Smooth`, hoy fijo 0,5-1 ms, relevante en Square) | S | **sí** |
| 8 | Profundidad por envolvente de entrada (amount guiado por el nivel, no solo la velocidad) | S-M | **sí** |

### (3) Diseño propuesto
Un visor grande (alto flexible) con dos ciclos del LFO (como hoy) y estas capas, de abajo arriba: rejilla de ciclos; curva R (secundario) y curva L (acento) rellena; cabezales vivos con punto en cada curva; **asa A** (círculo en el pico de L): arrastre horizontal = `Phase`, vertical = `Shape`; **asa B** (círculo en el pico de R): arrastre horizontal = `Stereo` (degrees o %, según Spin); **barra de Amount** en el borde derecho (ya existe). Rueda del ratón sobre el visor cambia el tiempo (Rate/Time/Division según `Time Mode`). Bajo el visor, una banda de 24 px: en Pan, escala -1…+1 con un marcador por canal (L en acento, R en secundario) que se mueve a la velocidad real (`meter()[0..2]`); en Tremolo, dos barras de ganancia. Debajo, una fila de chips de forma, selector de modo, `Time Mode`, `Stereo Mode`, `Invert`, y los interruptores de Harmonic/Vintage solo en Tremolo; y la fila de perillas (Tiempo, Stereo, Amount, Shape, Attack, Dyn FM, Mix, Out). Cuando existan, un botón `Draw` conmuta el visor a modo de dibujo (cursor lápiz, rejilla de 32 puntos, los controles Shape/Phase pasan a atenuarse).

---

## C.2 `fx.crusher`

### (1) Tabla comparativa
| Control / función | ¿Lo tenemos? | Dónde |
|---|---|---|
| Rate (Redux, Decimort, Kilohearts) | Sí (200-48000 Hz) | `crusher.rs`:`p::RATE` |
| Jitter (Redux, Decimort) | Sí, con modo enlazado/independiente | `p::JITTER, STEREO_JITTER` |
| Filtro previo y posterior, octava del posterior (Redux, Decimort) | Sí | `p::PRE, POST, POST_OCT` |
| Bits (Redux, todos) | Sí (1-16, pasos de 0,1) | `p::BITS` |
| Shape del cuantizador (Redux) | Sí | `p::SHAPE` |
| DC Shift (Redux) | Sí (+ `Keep DC`) | `p::DC_SHIFT, KEEP_DC` |
| Dry/Wet | Sí | `p::MIX` |
| **Dither** (Decimort, Kilohearts) | **No** | — |
| Mid-tread / mid-riser seleccionable (Decimort) | **No** (mid-rise solo por debajo de 2 bits) | `Quantizer::new` |
| Calidad de convertidores ADC/DAC (Kilohearts) | **No** | — |
| Ruido (TAL) | **No** | — |
| Drive de entrada antes de cuantizar (Krush) | **No** | — |
| Filtro resonante y LFO (Krush) | **No** | — |
| Curva de transferencia con asas | Sí (Bits) | ui:83 |
| Forma de onda con escalones y asa de Rate | Sí | ui:84 |
| Asa de DC Shift y barra de Nyquist (doc 18) | **No** | — |
| Espectro/alias en vivo | **No** | — |
| 14 presets | Sí | `crates/stock/presets/fx.crusher/` |

### (2) Cambios priorizados
| # | Cambio | Esfuerzo | DSP |
|---|---|---|---|
| 1 | **Dither** (`DITHER` 0-100 %, TPDF, antes del cuantizador) | S | **sí** |
| 2 | **Drive de entrada** (0-24 dB antes del cuantizador, sin cambiar el nivel de salida) | S | **sí** |
| 3 | Asa horizontal de DC Shift en la curva de transferencia y marcas de Nyquist en el plot de onda (frecuencia efectiva = Rate/2) | S | no |
| 4 | Línea de nivel en vivo sobre la curva de transferencia ya existe; añadir el punto "ahora" que recorre la curva (entrada/salida) con `meter()` | S | no |
| 5 | Selector Mid-tread / Mid-rise | S | **sí** |
| 6 | Mini espectro de 32-64 bins (de `meter()`, ≤ 320 valores) con el pico del alias marcado | M | **sí** (analizador en el hilo RT, sin asignaciones) |
| 7 | Ruido de fondo (`NOISE`), opción de pre-filtro con corte propio | S-M | **sí** |

### (3) Diseño propuesto
Dos visores lado a lado, como hoy, más un tercero opcional bajo ellos. **Izquierda: transferencia** (entrada en horizontal, salida en vertical) con la línea de identidad tenue, la escalera de cuantización en acento, los niveles retenidos por canal y dos asas: vertical derecha (Bits) y horizontal inferior (DC Shift), además de arrastrar la propia curva hacia arriba/abajo = `Shape`. **Derecha: onda de prueba interna** muestreada y retenida con bordes difusos por el jitter, asa horizontal inferior = Rate y una marca "Nyquist: x kHz" en el título. **Banda inferior** (si llega el espectro): 64 barras con el espectro de salida y una raya vertical en Rate/2 que se mueve al arrastrar Rate. Filas: píldoras Pre/Post/Keep DC/Stereo Jitter; perillas Rate, Jitter, Bits, Shape, DC, Dither, Drive, Post Oct (solo con Post), Mix, Out.

---

## C.3 `fx.buscomp`

### (1) Tabla comparativa (Glue de Live, SSL G-Bus, The Glue, Kotelnikov, MJUC)
| Control / función | ¿Lo tenemos? | Dónde |
|---|---|---|
| Threshold | Sí | `buscomp.rs`:`p::THRESHOLD` (-40..0) |
| Ratio 2/4/10 | Sí | `p::RATIO` (`RATIOS`) |
| Attack 0,01-30 ms en escalones | Sí | `p::ATTACK` |
| Release 0,1-1,2 s + Auto | Sí | `p::RELEASE` |
| Range | Sí | `p::RANGE` |
| Makeup | Sí | `p::MAKEUP` |
| Soft Clip (techo cerca de -0,5 dB) | Sí | `p::SOFT_CLIP`, `CEILING_DB` |
| Dry/Wet | Sí | `p::MIX` |
| Sidechain interno/externo, HPF, escucha | Sí | `p::SIDECHAIN, SC_HPF, SC_LISTEN` |
| Trim/Output | Sí | `p::OUTPUT` |
| Medidor de reducción (aguja/barra) y LED de clip | Sí | ui medidor alto + punto de clip |
| Historial de nivel y reducción | Sí (más que el mercado verificado) | ui:197 |
| Curva de transferencia con asas de Threshold y Makeup | Sí | ui:120 |
| **EQ de sidechain** (Live) / HPF de pendiente variable (Kotelnikov) | Parcial | solo HPF de 7 pasos |
| Release independiente para picos y RMS (Kotelnikov) | **No** | detector fijo 50/50 |
| Enlace estéreo / M-S | **No** | detector enlazado fijo |
| Auto-makeup (MJUC) | **No** | — |
| Autofade (Waves) | **No** | — |
| Oversampling seleccionable (Live: menú contextual) | Parcial | solo con Soft Clip |
| 10 presets | Sí | `crates/stock/presets/fx.buscomp/` |

### (2) Cambios priorizados
| # | Cambio | Esfuerzo | DSP |
|---|---|---|---|
| 1 | **Auto-makeup** (conmutador; compensa la reducción estática a -12 dB) | S | **sí** |
| 2 | Pintar en el plot de transferencia la curva del filtro de sidechain (respuesta del HPF) en un panel plegable | S | no |
| 3 | EQ de sidechain de una banda (campana o shelf) además del HPF | M | **sí** |
| 4 | Enlace estéreo 0-100 % y modo Mid/Side | M | **sí** |
| 5 | Mezcla Peak/RMS del detector (`Detect` 0-100 %) | S | **sí** |
| 6 | Mostrar el tiempo real del release en Auto en el propio mando (ya se muestra en una nota) | S | no |

### (3) Diseño
Mantener la composición actual (curva de transferencia con las asas, filas de pasos Ratio/Attack/Release, historial, medidor alto). Es el dispositivo más cercano a paridad. Añadir una tira plegable "SIDECHAIN" bajo los pasos con el selector de fuente, la curva del HPF/EQ y la escucha; un interruptor "Auto" junto a Makeup; y, si llega el enlace estéreo, un pequeño control `Link` junto a `Range`.

---

## C.7 `fx.drumbus`

### (1) Tabla comparativa (Drum Buss de Live, Smack Attack, Drum Gate, Saturate)
| Control / función | ¿Lo tenemos? | Dónde |
|---|---|---|
| Trim | Sí | `drumbus.rs`:`p::TRIM` |
| Compresor (on/off) | Sí, con cantidad | `p::COMP_ON, COMPRESS` |
| Drive con tipos Soft/Medium/Hard | Sí | `p::DRIVE, DRIVE_TYPE` |
| Crunch | Sí | `p::CRUNCH, CRUNCH_ON` |
| Damp | Sí | `p::DAMP, DAMP_ON` |
| Transients ± | Sí (bipolar único) | `p::TRANSIENTS, TRANS_ON` |
| Boom: cantidad, frecuencia, decay | Sí | `p::BOOM_AMOUNT, BOOM_FREQ, BOOM_DECAY, BOOM_ON` |
| **Force To Note** (frecuencia a la nota más cercana) | **No** | — |
| **Boom Audition** (solo del grave añadido) | **No** | — |
| Medidor del bajo | Sí (más rico: historial del grave de entrada y del boom, destello por golpe) | ui:81-110 |
| Output y Dry/Wet | Sí | `p::OUTPUT, MIX` |
| Interruptor por etapa | Sí (extra) | `p::*_ON` |
| Ataque y sustain separados con Sensitivity, Shape y Duration (Smack Attack) | **No** | un único `TRANSIENTS` |
| Guard: clip o limitador a -0,1 dBFS (Smack Attack) | **No** | — |
| Symmetry, Ceiling, escucha Delta (Saturate) | **No** | — |
| Compuerta con entrada externa/MIDI (Drum Gate 2) | **No** | fuera de alcance |
| Curva del drive visible | **No** | — |
| 10 presets | Sí | `crates/stock/presets/fx.drumbus/` |

### (2) Cambios priorizados
| # | Cambio | Esfuerzo | DSP |
|---|---|---|---|
| 1 | **Boom Audition** (`BOOM_SOLO`: salida = solo oscilador) | S | **sí** |
| 2 | **Force To Note**: botón que redondea `BOOM_FREQ` a la nota más cercana (editor, vía `Command`) y teclado de piano de una octava como regla bajo el visor del boom | S | no |
| 3 | Mini visuales por etapa: curva de transferencia del Drive (según tipo) con el punto de entrada vivo; barra de Transients con la ganancia aplicada (`meter()[6]`); reducción del compresor como barra; respuesta del Damp | M | no |
| 4 | Arrastrar `BOOM_DECAY` horizontalmente sobre el visor del boom (la x es tiempo) y `BOOM_FREQ` con rueda | S | no |
| 5 | **Delta** (escuchar solo lo añadido por Drive+Crunch) | S-M | **sí** |
| 6 | Dividir Transients en Attack y Sustain independientes (campos nuevos) | M | **sí** |
| 7 | **Guard** (clip suave a -0,1 dBFS) como etapa final | S | **sí** |

### (3) Diseño
Cadena de seis tarjetas en fila (Drive, Crunch, Comp, Trans, Boom, Damp), cada una con su interruptor y su mando principal, y debajo de cada título una **tira de 32 px** con su mini-visual vivo (ver cambio 3). El visor del Boom queda arriba, a ancho completo, con el grave de entrada en gris y el añadido en acento, el destello por golpe, la barra de Amount a la derecha y, nuevo, la regla de teclado con la nota de `BOOM_FREQ`. Botones bajo el visor: `Audition`, `Delta`, `Force to note`. Fila de salida: Trim, Mix, Output, latencia.

---

## C.4 `fx.mbdyn`

### (1) Tabla comparativa (OTT, Pro-MB, Multiband Dynamics de Live, MMultiBandDynamics)
| Control / función | ¿Lo tenemos? | Dónde |
|---|---|---|
| Crossovers arrastrables | Sí (2) | `p::XOVER_LOW, XOVER_HIGH`; ui:307 |
| Número de bandas | 3 fijas (Live: 3 con High/Low desactivables; Pro-MB hasta 6; MMBD 6) | `mbdyn.rs` |
| Umbral descendente y ascendente por banda, arrastrables | Sí | `p::band(b, TH_DOWN/TH_UP)`; ui:293-294 |
| Ratio descendente/ascendente | Sí, **no arrastrable** | `RATIO_DOWN/RATIO_UP`, solo en la fila (ui:325) |
| Floor, Max Up | Sí (extra), arrastrables | `FLOOR, MAX_UP` |
| Attack/Release por banda | Sí | `ATTACK, RELEASE` |
| Ganancia de banda, solo/mute | Sí | `BAND_OUT, MUTE, SOLO` |
| Depth por banda y global | Sí | `BAND_DEPTH, p::DEPTH` |
| Amount, Time, Up %, Down % | Sí | `p::AMOUNT, TIME, UPWARD, DOWNWARD` |
| Input / Output | Sí | `p::INPUT, OUTPUT` |
| Peak/RMS | Sí (global) | `p::DETECT` |
| Knee | Sí | `p::KNEE` |
| Modo lineal y latencia | Sí | `p::MODE` |
| Sidechain externo | Sí | `p::SIDECHAIN` |
| Nivel de entrada y ganancia aplicada en vivo | Sí | `meter()` 0..15 |
| Pico de salida por banda dibujado | **No** (existe en el meter, `METER_BAND_PEAK`, no se usa) | `mbdyn.rs`:`METER_BAND_PEAK` |
| Espectro de fondo | **No** | — |
| Curva de ganancia instantánea por banda sobre el medidor | Sí (más que el mercado verificado) | ui:150-215 |
| Lookahead (Pro-MB, hasta 20 ms) | **No** | — |
| Stereo link / M-S | **No** | — |
| Pendiente del crossover seleccionable (6-48 dB/oct) | **No** | LR4 fijo |
| 12 presets | Sí | `crates/stock/presets/fx.mbdyn/` |

### (2) Cambios priorizados
| # | Cambio | Esfuerzo | DSP |
|---|---|---|---|
| 1 | **Alt+arrastre vertical sobre el asa de umbral = ratio** (campo `Handle.alt`) y rueda = ratio | S | no |
| 2 | Dibujar el pico de salida por banda (marca fina) usando `METER_BAND_PEAK` | S | no |
| 3 | Ctrl/Cmd+arrastre aplica el cambio a las tres bandas; Alt+Mayús mueve umbral descendente y ascendente a la vez (convención de Live) | S | no |
| 4 | **Espectro de fondo** (64 bins en `meter()`: 207 + 64 = 271 ≤ 320) bajo el eje de frecuencias | M | **sí** (o analizador aparte) |
| 5 | Desactivar bandas alta/baja (2 o 1 banda) | M | **sí** |
| 6 | Stereo link y Mid/Side | M | **sí** |
| 7 | Lookahead opcional | M-L | **sí** (añade latencia) |

### (3) Diseño
Mantener el diseño de tres columnas (nivel + gráfico de ganancia + tira de ganancia + asas) y la barra de frecuencia. Añadir: (a) espectro semitransparente que cruza las tres columnas bajo la barra de crossovers; (b) conmutadores `Low`/`High` en la cabecera de banda para reducir el número de bandas; (c) la línea de ratio como segundo asa: pequeño rombo en cada línea de umbral cuya posición vertical muestra la pendiente. El panel lateral conserva Global (Amount, Time, Depth, Output, Up, Down, Knee, Input), Mode, Detect y el chip de sidechain.

---

# Síntesis

## Los diez cambios de diseño más importantes
1. **Stutter**: carriles apilados + pintar arrastrando + asa de longitud + copiar/pegar/rotar/×2 (editor, S-M).
2. **Stutter**: nivel y tape-stop **por paso** (campos 5-7 libres del stride) y swing (DSP, S-M).
3. **Stutter**: escenas A-D y recall por MIDI/automatización/teclas (modelo+DSP, L).
4. **Stutter**: Hold como pad momentáneo/latch, Randomize/Mutate como `Command`s (editor, S).
5. **Stutter**: rampas From-To (pitch, filtro, nivel) con la curva sobre el visor del trozo (DSP, M).
6. **Stutter**: disparo externo (transitorio/sidechain, nota MIDI) y modo ducking al bombo (DSP+motor, L).
7. **Autopan**: manipulación directa en el visor (Phase, Shape, Stereo, Rate) y banda animada de campo estéreo (editor, S); luego retrigger, onda dibujada y Haas (DSP, M-L).
8. **Crusher**: Dither y Drive de entrada (DSP, S) y asa de DC Shift + Nyquist (editor, S).
9. **Drumbus**: Boom Audition + Force To Note (S) y mini-visuales por etapa (M).
10. **Mbdyn**: ratio por Alt+arrastre, pico de salida por banda y espectro de fondo (S-M); buscomp: auto-makeup y EQ de sidechain (S-M).

## Cambios que tocan el DSP (no solo el editor)
Stutter 2, 3 (según opción), 5, 6, 7 ligadura; autopan retrigger/onda dibujada/Haas/Smooth; crusher Dither/Drive/Mid-rise/espectro/ruido; buscomp auto-makeup/EQ sidechain/link/RMS; drumbus Boom Audition/Delta/Guard/Attack-Sustain; mbdyn espectro/bandas desactivables/link/lookahead.

## Qué dispositivo queda más lejos del mercado
**`fx.stutter`**. Frente a Beat Repeat va por delante (rejilla, probabilidad, tape-stop), pero frente a Effectrix, Glitch 2 y Gross Beat le faltan los tres hábitos del género: carriles simultáneos con nivel/tape-stop por paso, escenas con lanzamiento por tecla y control en directo (Hold, aleatorizar, disparo externo). El segundo más lejano es `fx.autopan` frente a PanShaper (onda dibujada, disparo, Haas), aunque iguala a Auto Pan de Live. Buscomp, crusher, drumbus y mbdyn están a paridad o por encima de las referencias verificadas en su función base.

## Qué queda sin verificar
Disposición exacta de LFOTool, Infiltrator y Movement; el contenido de los manuales PDF; si el motor entrega `NoteOn` a un efecto de audio; Fade y Peak Clip de The Glue; Stutter de Kilohearts; TAL-Chorus-LX; el uso real por género de cada gesto.
