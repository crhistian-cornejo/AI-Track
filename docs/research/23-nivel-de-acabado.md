# 23 · Nivel de acabado: ZL Equalizer 2 y Chowdhury DSP frente a nuestros dispositivos

Fecha: 16-nov-2026. Trabajo de observación: no se tocó código de producción.

**Método.** Cada plugin se abrió con nuestro propio arnés (`crates/host/tests/real_plugins.rs`, `AITRACK_TEST_EDITOR_SECS`, sin sandbox) y se capturó la ventana real con `screencapture -l`. Nuestros dispositivos se capturaron con la app (copia del binario, motor simulado `AITRACK_STUBS=1`, `AITRACK_DEMO=1`, carpeta de config propia). Todo está en `…/scratchpad/ref/` (ruta completa al final).

**Marcas.**
* **[V]** = verificado en una captura que cito (archivo `zleq-…`, `o-…`) o en el código (grep).
* **[S]** = supuesto: lo sé de conocimiento previo del plugin o lo infiero; la captura no lo demuestra.
* Los porcentajes de superficie se midieron sobre las capturas (píxeles de la imagen, margen ±4 puntos); "área" = ventana sin la barra de título del sistema.
* Los tamaños de letra de los plugins externos se estiman por la altura de glifo en la captura (±1 px); los nuestros salen del código.

**Límites honestos.** No pude arrastrar ni hacer clic en los plugins externos (los eventos sintéticos no llegan; lo intenté en ZL EQ 2 con doble clic y no pasó nada). Por eso del ZL Equalizer 2 solo se ve el estado vacío (sin bandas) y todo lo que ocurre al seleccionar una banda es **[S]**. Tampoco se ve movimiento en vivo: el arnés deja de enviar audio cuando abre el editor y mis ocho capturas sucesivas de cada ventana son casi idénticas; en los nuestros el motor es simulado. Todo lo dicho sobre "qué se mueve" es **[S]** salvo lo que se ve congelado en un fotograma (medidores con valor, histograma de REEV-R).

---

## 1. Lo que se capturó

| Plugin | Ventana (pt) | Captura | Estado |
|---|---|---|---|
| ZL Equalizer 2 1.4.1 | 600×403 | `zleq-2720-10.png` | abre; vacío, sin bandas |
| ZL Compressor | 600×403 | `zlcomp-2730-10.png` | abre; **recortado abajo en nuestro host** |
| ZL Splitter 0.4.0 | 600×403 | `zlsplit-3172-10.png` | abre; vacío |
| CHOWTapeModel 2.11.4 | 720×652 | `tape-2734-10.png` | abre |
| BYOD 1.3.0 | 1000×632 | `byod-2733-10.png` | abre |
| ChowMatrix 1.3.0 | 800×632 | `matrix-2735-10.png` | abre |
| ChowCentaur 1.4.0 | 500×302 | `centaur-3178-10.png` | abre |
| ChowKick 1.2.0 | 850×632 | `kick-3175-10.png` | abre (la primera lectura dio 0×0, luego bien) |
| GATE-12 (Tilr) | 640×682 | `gate12-3173-10.png` | abre (relevante para Stutter/Shaper) |
| REEV-R (Tilr) | 690×682 | `reevr-3171-10.png` | abre |
| FILT-R (Tilr) | 640×682 | `filtr-3174-*.png` | abre (no analizado a fondo) |

Ninguno colgó el arnés. Todos abren y cierran dos veces sin problema.

**Hallazgos de alojamiento (nuestro host) [V]:**
1. Los editores ZL abren a 600×403 pt y el ZL Compressor sale con la fila de mandos cortada por abajo (`zlcomp-2730-10.png`: la etiqueta inferior de Makeup/Wet y el gráfico quedan cortados). La causa (tamaño que anuncia el plugin frente al que fijamos) es **[S]**.
2. ChowMatrix abre a 800×632 la primera vez y a 784×620 la segunda (`matrix.wins`): 16×12 pt de diferencia, probablemente la barra de título contada dos veces. **[S]** la causa, **[V]** la diferencia.

Capturas propias: `o-eq`, `o-compressor`, `o-gate`, `o-tape`, `o-stutter`, `o-shaper`, `o-multiband`, `o-reverb`, `o-kick808`, `o-arp`, `o-limiter`, `o-filter`, `o-sampler`, `o-wavetable` (todas `.png` en la misma carpeta).

---

## 2. Qué hacen bien (con medidas)

### 2.1 Reparto del espacio

| Dispositivo | Gráfico principal / área | Mandos visibles | Fuente |
|---|---|---|---|
| ZL Equalizer 2 | **~90 %** (rejilla de borde a borde) | 0 mandos de parámetro; barra de 8 elementos | `zleq-2720-10.png` [V] |
| ZL Compressor | **~92 %** visible (el panel de mandos *flota encima* del gráfico, ~20 % del área, fondo translúcido) | ~14 parámetros | `zlcomp-2730-10.png` [V] |
| ZL Splitter | ~90 % | 3 conmutadores FFT/MAG/WAV + selector de entrada | `zlsplit-3172-10.png` [V] |
| BYOD | ~88 % (lienzo de nodos) | pie con 8 campos | `byod-2733-10.png` [V] |
| GATE-12 | ~62 % | 7 mandos + 12 ranuras + 6 botones | `gate12-3173-10.png` [V] |
| REEV-R | ~52 % (dos gráficos) | 12 mandos + 12 ranuras + 8 botones | `reevr-3171-10.png` [V] |
| ChowTape | ~8 % (solo un visor IN/OUT) | 16 visibles + **7 pestañas con más** (Filters, Stereo, Comp, Tone, Degrade, CHEW, Wow) | `tape-2734-10.png` [V] |
| ChowMatrix | ~33 % (campo de nodos) + tabla | 7 filas × N nodos, con scroll | `matrix-2735-10.png` [V] |

Lectura: los ZL (el nivel que pide el dueño) son **gráfico-primero**: el dato es la pantalla y los mandos son un borde fino. Los Chow de efecto "de caja" (Tape, Centaur) son **mandos-primero**, pero con mandos grandes y pocos visibles; compensan con pestañas.

### 2.2 Jerarquía tipográfica (estimada) [V la imagen, S el px exacto]

* **ZL:** 3 tamaños (≈11 ejes/unidades, ≈13 etiquetas y barra, ≈15 etiquetas de mando) y un solo peso (medio) en una sola familia sans. Los valores van *dentro* del mando (compresor: -32, 3, 100, 500, 0, 100) o en la barra (`100%`, `0.0dB`).
* **Chow:** 4 tamaños (≈28 título negrita condensada, ≈14 cabecera de panel, ≈13 etiqueta, ≈12 valor) y 2 pesos. Valores bajo el mando con unidad (`0.00 dB`, `100%`).
* **Los nuestros [V, código]:** 414 usos de los 4 tokens de `theme.rs`; **365 (88 %) son `TEXT_XS` = 10 px**, 38 `TEXT_SM` (11), 6 `TEXT_MD` (12), 5 `TEXT_LG` (14). Cuatro pesos (normal 4, medio 18, seminegrita 35, negrita 2) y una segunda familia (Geist Mono) para los valores. Es decir: casi todo es el tamaño más pequeño que existe. En captura se nota: etiquetas "Thresh", "Att", "Rel" y todos los valores (`o-compressor.png`) son claramente más pequeños que las etiquetas de ZL.

### 2.3 Mandos, agrupación y rotulado

* **Tamaño de mando [V]:** ZL compresor ≈ 55 pt de diámetro; ChowTape ≈ 72 pt; Centaur ≈ 90 pt; ChowKick 40–60 pt. **Los nuestros: ≈ 33 pt** (anillo de 66 px en captura 2×, `o-compressor.png`).
* **ZL agrupa por proximidad y espacio, no por cajas**: en el compresor no hay ni un borde de grupo; Threshold/Ratio, Clean/Pump/Smooth, Attack/Release, Range/Hold, Makeup/Wet se separan por huecos de ~16 pt y por el cambio de tipo de control (botón vs mando). Chow usa **una sola caja con pestañas** por columna (`Gain|Filters|Stereo`) para esconder lo secundario.
* **Los nuestros [V]:** tres niveles de marco: panel exterior → tarjeta de grupo con borde (THRESHOLD, TIMING…) → mosaico de campo para algunos valores. Cuatro idiomas de control mezclados dentro del mismo dispositivo: mando, mosaico campo-con-subrayado (Depth/Smooth/Tension en `o-shaper.png`, Drive/Depth/Rate… en `o-filter.png`), desplegable y botones segmentados.
* **Rotulado:** ZL pone la etiqueta *encima* y el valor *dentro*; Chow, etiqueta encima y valor debajo con unidad. Siempre el mismo lado en todo el plugin. Los nuestros mantienen etiqueta arriba/valor debajo (bien), pero la etiqueta es de 10 px en gris y el valor monoespaciado de 10 px.

### 2.4 Qué se arrastra directamente sobre el gráfico

* **GATE-12 / REEV-R [V la presencia, S el gesto]:** nodos blancos, círculos naranjas pequeños para puntos de curva, círculo cian hueco para la tensión, modos `Paint | Seq | Curve`, `Grid 16`, `Snap`, flechas de desplazar el patrón y 12 ranuras de patrón numeradas. Todo el patrón se edita *sobre* el gráfico; los mandos son secundarios.
* **ZL EQ [S]:** doble clic en la rejilla crea banda, arrastrar la mueve, rueda cambia Q; el panel de la banda aparece al seleccionarla. No lo vi (vacío en la captura).
* **ZL Compressor [V presencia, S gesto]:** el gráfico es la curva de transferencia con la rodilla; en el borde izquierdo hay una regleta de 5 iconos de vista y un marcador blanco (triángulo) que parece el tirador de umbral. La curva termina en un punto (x≈735): lo más probable es que marque el nivel de entrada actual **[S]**.
* **BYOD [V]:** nodos Input/Output con un cable degradado naranja→azul; `+` para añadir bloque; zoom 100 %.
* **Los nuestros [V]:** ya hay manejadores en la curva (umbral/rodilla en compresor y gate, bandas del EQ, nodos del shaper, envolventes del sampler/kick/wavetable, trazos del arpegiador). Aquí no perdemos en *existencia*, sí en *acabado* (ver §3).

### 2.5 Valores y unidades

* ZL: sin unidad dentro del mando (`-32`, `3`), unidad en la barra (`0.0dB`, `100%`). Chow: `0.00 dB`, `15.00` con unidad en el rótulo (`Speed [ips]`, `Gap [microns]`), y `Default*` con asterisco cuando el preset se ha tocado **[V]**.
* Nuestros: siempre valor con unidad en mono (`-18.0 dB`, `120.0 ms`, `20.00 kHz`) — bien y consistente. Falla la talla (10 px) y que el valor "en vivo" mientras arrastras no se amplía.

### 2.6 Barra superior y pie

* **ZL EQ [V]:** 3 iconos de vista (analizador, curvas, biblioteca), `Analyzer`, un icono de modo, centrado `Minimum Phase`, a la derecha `100%` (fuerza), `0.0dB` (ganancia global), icono de auto-escala, encendido. **ZL Compressor [V]:** los 3 iconos, `Compress`, `Clipper 0`, `Oversample Off`, `Lookahead 0`, dos lecturas bajo un icono triángulo (`0.5`, `-6.5` **[S]** reducción/nivel), encendido, y una **línea fina magenta bajo la barra** (probable medidor de nivel de 1 px **[S]**).
* **Chow [V]:** pie con `Oversampling: 2x`, `Hysteresis Mode: NR4`, `Mix Group: N/A`, flechas de preset, `Default*`, engranaje. **BYOD [V]:** pie con deshacer/rehacer, `Mono ▾`, `Input/Output/Dry-Wet` como campos con unidad en color de acento, `2x ▾`, flechas de preset, **`1%` de CPU**, engranaje, y botones de zoom. **ChowMatrix [V]:** `Dry`, `Wet`, `Mode: Smooth`, `Clear`, `Free`, flechas, `Default*`, **`A > B`**, dado (aleatorizar), engranaje; y un deslizador `Insanity` con botón de reset.
* **Los nuestros [V]:** cabecera propia más completa que la de ZL (encendido, icono+nombre+pista+categoría, selector de preset con flechas, guardar, reiniciar, A/B, copiar) y pie con `LATENCY`, `DSP` con barra, `RATE`. En esto **estamos a la par o por delante**; solo falta oversampling/modo/zoom en el pie.

### 2.7 Redimensionado

* Chow y BYOD muestran **tirador de redimensión** abajo a la derecha (`centaur`, `byod`, `matrix`, `tape` [V]) y BYOD zoom 100 % con + y − [V]. ZL no muestra tirador [V]; escalado por proporción **[S]**.
* **Los nuestros [V código]:** paneles fluidos (`flex_1`, `min_w`) en ventana, y tamaño recordado por plugin. Sin escala de interfaz. En las capturas el sobrante de ancho se reparte mal: los grupos se quedan a la izquierda y queda hueco (`o-limiter`, `o-tape`, `o-compressor`, `o-gate`, `o-stutter`).

### 2.8 Detalles que los hacen "caros" [V salvo indicación]

1. **Fondo cálido casi negro y rejilla casi invisible.** ZL: fondo `#151009`, rejilla `#1e1a15` (solo ~+4 de luminosidad: la rejilla se *intuye*). Nuestro gráfico `#101115` con rejilla más marcada y etiquetas grandes de eje que compiten con la curva.
2. **Un único color de datos y mucho aire.** ZL compresor: *una* línea naranja sobre el vacío. Nosotros: curva + relleno + 2 manejadores de colores distintos + 3 trazas con leyenda.
3. **Mandos con volumen.** Los Chow tienen degradado y reborde interior (aspecto físico); los ZL, anillo con sombra suave y el valor dentro. Los nuestros son anillo plano de 3 px con una raya.
4. **Texto blanco real sobre superficie oscura** (`#e7e6e5` medido en ZL) frente a etiquetas grises pequeñas en las nuestras.
5. **Cabeceras de grupo claras** (ChowTape: título en negrita en una barra con pestañas separadas por una línea de 1 px).
6. **Estado "tocado"**: `Default*` (Chow) y botón de reset (`Insanity` en Matrix).
7. **Superficies que parecen bloques**: Chow pinta cada panel con su propio fondo y radios ~8 pt; no hay marcos.
8. Los estados de foco y *hover* de los externos **no son verificables** en captura estática **[S]**.

**Y sus fallos (para no copiarlos) [V]:** ZL EQ vacío sin pista de "doble clic para añadir banda"; ChowTape deja ~14 % de la ventana en rojo vacío; ChowKick pisa `Noise Amount` con `Noise Decay` y `Noise Cutoff`; ZL Compressor recortado al cargar en nuestro host.

---

## 3. Dónde perdemos (sin rodeos)

1. **Gráfico demasiado pequeño en casi todo.** Medido: Limiter 23 %, Sampler 28 %, Compressor 31 %, Reverb 31 %, Tape 36 %, Stutter 43 %, Gate 48 %, Kick 808 48 %, Multiband 49 %, Arpegiador 52 %, Filtro 55 %, Shaper 59 %, EQ **66 %** (el mejor). ZL: 90 %. GATE-12: 62 %. Wavetable: ≈20 %. [V, capturas `o-*`]
2. **Letra de 10 px en el 88 % de los textos** y 4 pesos + mono: parece panel de depuración. [V código]
3. **Mandos de 33 pt**, la mitad que ZL/Chow. Con 16–37 por pantalla hay que hacerlos pequeños; la solución es tener *menos a la vista*, no mandos diminutos. [V]
4. **Demasiados controles visibles a la vez:** Compresor 16 mandos + 13 botones (~29), Stutter ~28, Reverb ~37, Wavetable ~90; **ninguno esconde nada** salvo el sintetizador por pestañas. ZL Comp ~14, ZL EQ 0 hasta seleccionar. [V recuento en capturas]
5. **Tres niveles de marco + cuatro idiomas de control** mezclados. [V]
6. **Espacio muerto** en la derecha/abajo de Limiter, Tape, Gate, Compressor, Stutter y Reverb: los grupos tienen ancho fijo y se quedan a la izquierda. [V]
7. **Colisiones de texto con manejadores/curvas** (defectos, no gusto): "+0.0" encima del manejador y "200" sobre la línea de cruce en Multiband (`o-multiband.png`); "C4" fantasma sobre el primer paso y "velocity · as played" tapado por las barras en el Arpegiador (`o-arp.png`); "ceiling -1.0 dB" tapado por el punto en Limiter (`o-limiter.png`); "pre 10 ms" bajo el punto amarillo en Reverb (`o-reverb.png`); "A 0.0 ms" bajo el manejador y "10k|20k" pegados en Wavetable y Sampler (`o-wavetable.png`, `o-sampler.png`). [V]
8. **Sin lectura flotante en el manejador** al arrastrar (en nuestros hay etiquetas fijas como "-18.0 dB" pero no una burbuja que sigue al cursor) **[S]**: no lo pude probar sin clics; el código de `kit.rs` no muestra tooltip con unidad y valor por mando (2 `.hover(` en `kit.rs`) [V código].
9. **Sombras y relieve ausentes** (un `shadow` en `eq.rs`, `sampler.rs`, `synth/knob.rs`); los mandos no tienen volumen. [V código]
10. **Nuestro motor simulado no mueve nada**, así que el aspecto "vivo" no está comprobado ni para bien ni para mal. [S]

**Donde sí vamos bien (y hay que proteger):** cabecera con A/B + presets + copiar (mejor que ZL y Chow), pie con latencia/DSP, consistencia de valores con unidad, cada dispositivo tiene ya una visualización firma (transferencia + historial en el compresor, saturación + respuesta en Tape, histórico de eventos en Stutter), y el Kick 808 (`o-kick808.png`, 48 % de gráfico, etiquetas sobre la curva, unidades) ya está cerca de ChowKick en estructura y por delante en limpieza.

---

## 4. Especificación de acabado (aplicable a TODOS los editores)

Todo en píxeles lógicos a 1×, múltiplos de 4 salvo texto. Tokens nuevos en `crates/app/src/theme.rs` solo para dispositivos (prefijo `DEV_`) para no mover el resto de la app.

### R1 · Proporciones (la regla que más se nota)
* Reparto del cuerpo (entre cabecera y pie): **gráfico firma ≥ 60 %**, mínimo absoluto 50 % si el dispositivo tiene más de 12 controles; **tira de mandos ≤ 25 %**; el resto, aire.
* La tira de mandos es **una sola fila** de ≤ 8 mandos primarios (altura ≈ 112: padding 12 + etiqueta 16 + mando 44 + valor 18 + 12). Lo demás va a un cajón "Más" (plegable, recordado por dispositivo) o a pestañas de ≤ 3 por columna (como Chow).
* **Todo el sobrante de ventana va al gráfico**: la altura sobrante, a `h` del gráfico; el ancho sobrante, a `w` del gráfico, y los grupos de mandos se reparten con `justify-between` sobre todo el ancho (nada de grupos arrimados a la izquierda con hueco a la derecha).
* Opcional para dispositivos de dinámica y filtros: tira de mandos *flotante sobre el gráfico* (fondo `surface-1` al 88 %, radio 8, borde 1 px) como ZL Comp; así el gráfico llega al 90 %.
* Instrumentos y sintes (Wavetable, Sampler, Vault, Acid303): ≥ 1 gráfico firma ≥ 40 % más pestañas; nada con ~90 controles en una pantalla.

### R2 · Escala tipográfica
Cuatro tamaños, tres pesos, una sola familia sans; mono solo para valores numéricos.

| Rol | Tamaño | Peso | Uso |
|---|---|---|---|
| `DEV_TITLE` | 16 | 600 | nombre del dispositivo (cabecera), una vez |
| `DEV_VALUE` | 13 | 600, mono tabular | valor bajo el mando, lecturas, campos |
| `DEV_LABEL` | 12 | 500 | etiqueta de mando/botón/pestaña, texto de desplegable |
| `DEV_GROUP` | 11 | 600, MAYÚSCULAS +0.04 em | cabecera de grupo, rótulos de lienzo |
| `DEV_AXIS` | 11 | 400 | ejes, unidades, pies, estado |

* **Mínimo 11. Nada de 10.** Hoy 365 usos de 10 px pasan a 11 (axis/group) o 12 (label).
* Color de texto: valor `text` pleno; etiqueta `text` al 72 %; eje `text` al 55 %. Contraste mínimo 4.5:1 sobre su superficie (comprobar con la herramienta, no a ojo).
* Como máximo 1 línea de texto de 16 px por ventana.

### R3 · Mandos
* **Primario Ø 44, secundario Ø 36, mínimo Ø 28 solo en matrices densas** (Matrix de modulación del synth). Hoy ≈ 33.
* Anillo 3 px (4 px en primario), indicador de 2 px; relleno del anillo en el color de acento desde la posición "por defecto" (bipolares desde el centro); sombra interior sutil y 1 px de luz arriba (efecto Chow); nada de degradados saturados.
* Hit area = mando + etiqueta + valor, mínimo 44×56.
* Columna de mando ≥ 64 de ancho, gap entre columnas 12; etiqueta **encima** (12/500), valor **debajo** (13/600 con unidad). Mismo lado en todo el dispositivo.
* Un único idioma por tipo de dato: **continuo = mando**, **enumerado ≤ 4 = segmentado**, **enumerado > 4 = desplegable**, **booleano = botón con LED**. Se elimina el "mosaico campo con subrayado" de Shaper/Filtro/Arp: pasa a mando o a campo numérico de la misma altura que un botón (28).

### R4 · Densidad y agrupación
* ≤ 12 controles de parámetro a la vista en el modo normal; ≤ 24 contando cajones plegados; ≥ 25 → obligatorio pestañas o cajón.
* **Dos niveles de superficie, no tres**: lienzo del dispositivo (`surface-0`) → tarjeta de grupo (`surface-1`, radio 8, **sin borde**, padding 12). El mosaico de campo desaparece. Cabecera de grupo `DEV_GROUP`. El borde de 1 px solo para foco y popovers.
* Grupos de ≥ 3 y ≤ 6 mandos; gap entre grupos 12. Si un grupo tiene 1 solo control, se funde con el vecino.

### R5 · Estados
| Estado | Mando/botón | Manejador en gráfico |
|---|---|---|
| reposo | anillo `surface-2`, relleno acento 100 % | punto Ø 12, borde 2 px blanco 90 % |
| hover | superficie +6 % luminosidad; cursor `ns-resize` (mando), `grab` (manejador); tooltip a 400 ms con nombre completo, valor, unidad, por defecto, atajo | punto Ø 16 con halo 4 px acento al 30 % |
| activo (arrastrando) | burbuja de 16/600 sobre el mando con el valor en grande; anillo a brillo 100 % | burbuja que sigue al cursor con las dos magnitudes (p. ej. `1.2 kHz · +3.5 dB`) y líneas guía a los ejes |
| foco de teclado | anillo de 2 px acento al 70 %, separación 2 px | igual |
| desactivado | 40 % de opacidad, sin hover | 40 % |
| modulado | arco exterior de 2 px con el color de la fuente de modulación | — |
| con automatización | punto de 4 px rojo bajo la etiqueta | — |
| distinto del preset | asterisco `*` tras el nombre del preset y punto de 4 px bajo el valor (copia de `Default*`) | — |

Interacción común: doble clic = valor por defecto; Mayús/Alt = ajuste fino ×0.1; rueda = paso; clic sobre el valor = teclear; clic derecho = menú (automatización, aprender MIDI, copiar valor). Manejadores con zona de clic de 24 px aunque se dibujen de 12.

### R6 · Qué debe moverse en vivo (obligatorio, mínimo uno por dispositivo)
| Familia | Elemento vivo mínimo |
|---|---|
| EQ / filtro | analizador pre/post (FFT 4096, suavizado ¼ oct, 30 fps, caída 12 dB/s) bajo la curva, con el color de la curva al 20 % |
| Compresor / gate / limitador | punto de operación sobre la curva de transferencia, historial entrada/salida/GR desplazándose, GR en el carril con retención de pico 1.5 s |
| Saturador / tape / amp | señal entrando sobre la curva de saturación (punto + traza corta), medidor IN/OUT |
| Reverb / delay | envolvente de decaimiento con la cola "encendiéndose" al entrar señal; taps del delay iluminados al sonar |
| Stutter / shaper / gate rítmico | cabezal de reproducción sobre el patrón (como un playhead de pasos), paso activo resaltado |
| Multibanda | nivel por banda dentro de su bloque, GR por banda |
| MIDI (arp, chord…) | notas entrando/saliendo iluminadas en el lienzo |
| Instrumentos | voces activas sobre el gráfico de onda/envolvente, medidor de salida |
Refresco ≥ 30 fps con interpolación; **nada se redibuja si el dispositivo está en bypass**; todo debe tener caída visible (no saltos).

### R7 · Cabecera y pie
* Cabecera 48: encendido, nombre (16/600) + `pista · categoría` (11), preset con flechas y `*`, guardar, reiniciar, A/B, copiar — **ya existe, mantener**. Añadir a la derecha: botón **Delta/escucha** y **ganancia de salida** cuando aplique.
* Pie 24: latencia, DSP con barra, frecuencia (ya existe); añadir a la izquierda **sobremuestreo** y **modo** del dispositivo si lo tiene (ejemplo Chow: `Oversampling: 2x`) y **zoom** (R9).
* Medidores IN/OUT: carril de 2×8 px con escala compartida de 28 px de ancho, **solo si el dispositivo procesa audio**; hoy ocupan ≈ 90 px de ancho y se repiten.

### R8 · Superficies y color
* Escala de superficies (muestreada en `o-compressor.png`): `surface-0` `#101115` (lienzo, gráfico), `surface-1` `#1b1c20` (tarjeta), `surface-2` `#222327` (campo/botón). Mantener; los colores salen del tema (`theme.rs`), nunca `rgb(0x…)`.
* **Rejilla del gráfico**: líneas a +4–6 puntos de luminosidad sobre el fondo del gráfico (`#1e1a15` sobre `#151009` en ZL), 1 px; **líneas mayores** (0 dB, 1 kHz, tiempo 1 s) +10. Etiquetas de eje 11/400 al 55 %, **nunca sobre la curva**: se desplazan al borde o se ocultan si colisionan (resolver las colisiones de §3.7).
* **Un solo color de datos principal por dispositivo** (el acento) y ≤ 2 secundarios (entrada, GR). El relleno bajo la curva ≤ 12 % de opacidad.
* Elevación: tarjetas sin sombra, 1 px de luz arriba (`white 4 %`); flotantes (popover, burbuja, tira flotante) con sombra `0 8 24 black 35 %`. Radios: 4 control, 8 tarjeta, 12 ventana.

### R9 · Redimensionado
* Tamaño mínimo = contenido mínimo (hoy `placement::editor_extent`); por debajo, scroll vertical con barra visible, nunca recorte.
* Al agrandar: gráfico elástico (R1). Los mandos **no** escalan con la ventana.
* **Escala de interfaz** 75–150 % (pasos de 25) en el pie, recordada por plugin; tirador de esquina para tamaño, como Chow/BYOD [V].
* En alojamiento de plugins externos: respetar el tamaño anunciado + borde de ventana y no recortar (hallazgo ZL Compressor).

### R10 · Texto y etiquetas sobre gráficos
* Una colisión entre texto y manejador/curva es un defecto bloqueante: el motor de etiquetas del gráfico debe tener un `place_label(rect, avoid: &[Rect])` que pruebe 4 esquinas y oculte si no hay hueco. Pruebas unitarias con los casos de §3.7.

### R11 · Dispositivos en ventana: criterio de aceptación
Un editor pasa el listón si, en captura a la ventana por defecto: (1) gráfico ≥ 60 %; (2) ningún texto < 11 px; (3) ≤ 12 parámetros visibles o cajón; (4) sin tarjeta con borde dentro de tarjeta; (5) sin hueco muerto > 8 % del área; (6) cero colisiones texto/manejador; (7) existe el elemento vivo de R6; (8) mando primario ≥ 44.

---

## 5. Dispositivo por dispositivo (lo que falta), ordenado por lo que más se nota

Gráfico = % del área medido. "Reglas" remite a §4. Capturas: [V] las `o-*.png`; los que no se capturaron son [S] y salen del código/registro.

| # | Dispositivo | Gráfico | Qué más se nota (en orden) | Reglas | Evidencia |
|---|---|---|---|---|---|
| 1 | **Limitador** | 23 % | Gráfico mínimo, panel LOUDNESS casi vacío (`—`), hueco muerto a la derecha de ENVELOPE/OUTPUT, 4 tarjetas con borde, "ceiling" tapado por su punto | R1, R4, R6, R10 | `o-limiter.png` [V] |
| 2 | **Wavetable Synth** | ~20 % | ~90 controles en una pantalla, 3 osciladores idénticos, 2 filtros, envolventes: mismo peso visual para todo; texto 10 px; etiquetas sobre manejadores | R1 (pestañas), R3, R4 | `o-wavetable.png` [V] |
| 3 | **Sampler** | 28 % | Onda vacía ocupa menos que la botonera; filtro y envolvente en miniatura; `10k 20k` pegados; mosaico + mandos mezclados | R1, R3, R5, R10 | `o-sampler.png` [V] |
| 4 | **Compresor** | 31 % | Dos gráficos pequeños frente a 29 controles; sidechain/detector fuera de la vista principal; hueco abajo a la derecha; sin punto de operación vivo | R1, R4, R6 | `o-compressor.png` [V] |
| 5 | **Reverb** | 31 % | 37 controles; 9 algoritmos + 3 modos + 3 colores en botones; hueco en el pie; "pre 10 ms" tapado | R1, R3, R4, R10 | `o-reverb.png` [V] |
| 6 | **Tape** | 36 % | Mandos de 33 pt en 4 tarjetas, hueco grande abajo-derecha; ChowTape es mandos-primero con pestañas y 72 pt | R1, R3, R4 | `o-tape.png` [V] |
| 7 | **Delay** | ~30 % [S] | Misma plantilla que Reverb (capturado: `o-delay.png`; no analizado a fondo) | R1, R4 | `o-delay.png` capturado, no inspeccionado [S] |
| 8 | **Stutter** | 43 % | Es el más denso (≈28): dos lienzos grandes bien, pero 6 tarjetas de campos pequeños y huecos abajo; GATE-12 hace lo mismo con 7 mandos + patrón | R1, R3, R4, R6 | `o-stutter.png` [V] |
| 9 | **Bus Comp / Drum Bus / Console / Amp / Saturator** | ~30–40 % [S] | Misma plantilla que Compresor/Tape (capturados `o-buscomp.png`, `o-saturator.png`) | R1, R3, R4 | `o-buscomp`, `o-saturator` capturados, no inspeccionados [S] |
| 10 | **Gate** | 48 % | Carril GR a la derecha muestra `-40.0` en rojo lleno (en stub); hueco abajo derecha; mismos problemas que el compresor | R1, R6 | `o-gate.png` [V] |
| 11 | **Kick 808** | 48 % | **Bien.** Mejora menor: tarjetas con barra de color y texto 10 px; burbujas de valor sobre la curva ya existen | R2, R3 | `o-kick808.png` [V] |
| 12 | **Multibanda** | 49 % | Colisiones ("+0.0" sobre el manejador, "200" sobre el cruce), controles por banda a la izquierda con hueco enorme a la derecha | R1, R10 | `o-multiband.png` [V] |
| 13 | **Arpegiador** y MIDI (chord, scale, echo, pitch, velocity, length, random) | 52 % | Buen lienzo; "C4" fantasma, rótulo tapado por barras; controles de 10 px; los demás MIDI son [S] (no capturados ahora) | R2, R4, R10 | `o-arp.png` [V]; resto [S] |
| 14 | **Filtro** | 55 % | Dos idiomas de control + hileras de mosaicos; curva sin analizador vivo | R3, R4, R6 | `o-filter.png` [V] |
| 15 | **Shaper** | 59 % | Cerca del listón; mosaicos → mandos, sin cabezal vivo | R3, R6 | `o-shaper.png` [V] |
| 16 | **EQ** | 66 % | Cerca del ZL: bandas 1–8 como píldoras, menú de fase, pie. Falta: analizador vivo, panel de banda al seleccionar, burbuja al arrastrar; las píldoras tienen 10 px | R5, R6, R2 | `o-eq.png` [V] |

Los dispositivos sin captura directa (Analyzer, Utility, Mid/Side, Crusher, Chorus, Autopan, Clipper, Transient, Drum Extract, Vault, Acid303, Drum) heredan la plantilla; se miden con el criterio R11 cuando se rehagan. [S]

---

## 6. Qué copiar (comportamiento, no aspecto) y qué podemos hacer mejor

### 6.1 Copiar como comportamiento

1. **Gráfico-primero con la tira de mandos flotando encima** (ZL Compressor) [V]: gana 20 puntos de superficie sin esconder nada.
2. **Cero mandos hasta seleccionar** (ZL EQ) [V el vacío, S el panel contextual]: el panel de la banda aparece solo al elegirla. Nuestro EQ ya tiene 8 bandas como píldoras: añadir el panel contextual y quitar controles permanentes.
3. **Creación por gesto en el gráfico**: doble clic en la rejilla crea banda; arrastrar fuera la quita [S].
4. **Pestañas para lo secundario, tres por columna** (ChowTape `Gain|Filters|Stereo`) [V].
5. **Preset "tocado" con asterisco** (`Default*`) en Chow y Matrix [V].
6. **Pie utilitario**: sobremuestreo y modo siempre visibles; CPU% (BYOD `1%`) [V]. Nosotros ya tenemos DSP%; falta sobremuestreo/modo.
7. **Deshacer/rehacer y zoom a mano en el pie** (BYOD) [V]. Nosotros ya tenemos `Command` + deshacer global: exponer ⌘Z dentro de la ventana (ya se reenvía) y un botón visible.
8. **A > B con morph** (ChowMatrix) [V]: no solo conmutar, también interpolar.
9. **Aleatorizar con intensidad** (`Insanity` + dado) [V].
10. **Patrones numerados 1–12 en fila con sincronía** (GATE-12/REEV-R) [V]: para Stutter, Shaper y los MIDI rítmicos.
11. **Paint | Seq | Curve + Grid + Snap** como selector de herramienta del lienzo [V].
12. **Tirador de redimensión visible** [V].

### 6.2 Hacerlo mejor que ellos (lo que pide el dueño)

1. **Contenido ya "vivo" y verificable**: ni ZL ni Chow muestran de dónde viene cada número. Nuestra burbuja de arrastre (R5) debe dar frecuencia + ganancia + **nota musical** más cercana y la banda; en dinámica, ratio + umbral + reducción real a la vez.
2. **Todo es `Command`** (regla del repo): deshacer/rehacer ilimitado, historial con nombre, y **el asistente de IA** puede proponer y aplicar ajustes del dispositivo visibles en el propio gráfico (ghost curve A/B). Es una ventaja estructural que ninguno de los dos tiene.
3. **Automatización y modulación desde el propio mando** (clic derecho → "Mostrar carril", arrastrar una fuente sobre el mando; el sintetizador ya lo hace: "Drag the handle onto a knob"): ZL tiene automatización de host, no modulación visible.
4. **Escucha Delta y solo por banda** en EQ, compresor y multibanda con un clic, también en el limitador.
5. **Analizador pre/post con sidechain/otra pista** (la lista de pistas ya está en el proyecto) y medición entre dos pistas (colisión de frecuencias: ya existe `eq/collision.rs`).
6. **Zoom de interfaz 75–150 %, accesibilidad completa por teclado** (foco visible, flechas = pasos) y tooltips con unidad y valor por defecto: ZL/Chow no los muestran [S].
7. **Sin los fallos que sí tienen**: sin mandos pisándose (ChowKick), sin vacío sin pista (ZL EQ vacío: "doble clic para añadir banda"), sin 14 % de relleno muerto (ChowTape).
8. **Densidad adaptable**: el mismo dispositivo en tarjeta de la pista, en ventana normal y en ventana grande con tres niveles de detalle (ya existe `Layout::Window`); ellos solo tienen un tamaño y escalan.

---

## 7. Orden de trabajo sugerido (para repartir entre agentes)

1. **Tokens y kit** (`theme.rs`, `device_ui/kit.rs`): `DEV_*` tipografía R2, mando Ø 44 R3, tarjeta sin borde R4, estados R5. Afecta a todos a la vez y es lo que más sube la sensación de calidad por línea de código.
2. **Reparto de espacio** (R1) en `device_window/mod.rs` + `kit::panel`: gráfico elástico y grupos `justify-between`. Quitar huecos muertos de Limiter/Tape/Compressor/Gate/Reverb/Stutter.
3. **Motor de etiquetas** con evitación (R10): arreglar las 8 colisiones de §3.7 con una utilidad común.
4. **Elementos vivos** (R6) por familia, empezando por analizador del EQ y punto de operación del compresor/gate.
5. Plantillas con cajón "Más": Compresor, Reverb, Stutter, Wavetable.
6. Host: tamaño de ventana de plugins externos (ZL Compressor recortado, Matrix 16×12).

---

Capturas y scripts: `/private/tmp/claude-501/-Users-corx-Developer-AI-Track/2f35022c-3710-4989-a632-a34b1be5a151/scratchpad/ref/` (`cap.sh` abre y captura un plugin; `ours.sh` captura un dispositivo nuestro; `px.swift` muestrea píxeles).
