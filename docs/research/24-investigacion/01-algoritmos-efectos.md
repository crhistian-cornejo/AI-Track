# 24.01 · Algoritmos de nuestros efectos frente al estado del arte (open source y académico)

Fecha: 17-nov-2026. Trabajo de investigación: no se tocó código de producción, no hay commit. Las mediciones salen de una sonda desechable fuera del repositorio (`~/.capy/work/fxprobe`, ver §11).

**Leyenda.** **[V]** = verificado: lo leí en el código (con `ruta:línea`), lo medí con la sonda, o lo leí en la fuente citada. **[S]** = supuesto o inferencia sin fuente directa. Licencias: se marcan en cada referencia; las GPL/LGPL/AGPL solo se estudian, la recomendación siempre es una implementación propia o una dependencia permisiva que ya admite `deny.toml`.

---

## 0. Resumen

La base es buena y en varios sitios está a la altura de las referencias libres: EQ con biquads ajustados de Vicanek y modos FIR Natural/Lineal, compresor de Giannoulis et al. con topología feedback estable, limitador con mínimo deslizante y detector true-peak 4×, reverb FDN con RT60 exacto por banda y placa de Dattorro, delay con lecturas sinc de 8 taps, saturación ADAA1 con sobremuestreo IIR polifásico, y un warp con seis modos. Lo que este informe añade a los anteriores son **cuatro defectos medibles** que las referencias no tienen, y que se arreglan con poco código:

1. **El Clipper tira por la borda su sobremuestreo.** Después de diezmar recorta otra vez a la tasa base (`crates/stock/src/fx/clipper.rs:236`). Medido con un seno de 4987 Hz recortado: el aliasing pasa de −80,9 dB (antes del recorte final, 4×) a −26,0 dB (salida real); a 1× es −25,5 dB. Hoy 2× y 4× solo añaden latencia. [V]
2. **Transponer hacia arriba aliasa al 100 %** en el warp (Complex, Complex Pro, Tones y Re-Pitch) y en el Pitch Shifter. Un seno de 13 kHz subido una octava sale a 22 kHz con el mismo nivel, cuando debería desaparecer (26 kHz > Nyquist). La causa es una interpolación cúbica sin filtro antialias (`crates/dsp/src/warp.rs:56-69`, `crates/stock/src/fx/pitchshift.rs:268`). Signalsmith Stretch (MIT) deja ese residuo en −75,4 dB. [V]
3. **El limitador a 1× (el valor por defecto) supera el techo true-peak** hasta +0,35 dB en material ya recortado, porque el detector 4× de 16 taps por fase pierde 0,49 dB a 18 kHz y 2,1 dB a 20 kHz. A 2× y 4× cumple. [V]
4. **El time-stretch "Complex" queda 12 dB por detrás de Signalsmith Stretch** en un acorde estacionario (energía espuria −17,3 dB frente a −29,5 dB), con 3,3 ms de jitter en la posición de los transitorios y −13,5 dB de pre-eco; Signalsmith está incluso en `PLAN.md:136` como dependencia prevista y no se usa. [V]

A eso se suma una deuda de diseño ya conocida pero sin cuantificar: los filtros TPT/SVF (bandas dinámicas del EQ, modo Classic, cortes, filtros del sidechain) se "aprietan" cerca de Nyquist: una campana de +12 dB en 12 kHz pierde 4,2 dB a 18 kHz y el error llega a 6,2 dB con la campana en 16 kHz, frente a menos de 1 dB con el diseño ajustado. [V]

La lista priorizada está en §10. Las cinco primeras mejoras son de esfuerzo S o M y no tienen riesgo de licencia.

---

## 1. Qué ha cambiado desde los informes anteriores

| Informe | Afirmación | Estado hoy |
|---|---|---|
| 19 A2 / 20 trabajo 1.12 | El warp se calcula síncrono dentro de `compile()` | **Resuelto.** `AudioClipData::of_bpm_in` pide el render a `SampleCache::warped`, que lo encola en un worker (`crates/engine/src/sample_cache.rs:657`) y mientras tanto el clip suena en Re-Pitch (`crates/engine/src/clip.rs:334-347`). [V] |
| 14 D5 | Warp ON con `original_bpm == tempo` sale del camino exacto | **Resuelto en el modelo de datos**: hay un indicador `unity` (`crates/engine/src/clip.rs:328, 355`). No re-medí el camino de lectura. [V código / S efecto] |
| 18 §crusher | El `Bitcrusher` no tiene jitter | **Resuelto**: `fx.crusher` tiene reloj con jitter, prefiltro y posfiltro (`crates/stock/src/fx/crusher.rs:1-8`). [V] |
| 12 (índice) | `fx.filter` = "SVF TPT / ladder" | **No es así**: solo SVF lineal de 12/24 dB con un `tanh` ADAA *antes* del filtro (`crates/stock/src/fx/filter.rs:1-2, 245-248`). No hay ladder ni no linealidad en la realimentación. [V] |
| 12 reverb-delay | "8-tap windowed-sinc reads" en reverb y delay | Cierto para reverb (`crates/stock/src/fx/reverb.rs:255, 300`) y delay (`crates/stock/src/fx/delay.rs:450, 501`). **No** para chorus/flanger, Pitch Shifter ni el shimmer, que siguen con Hermite o lineal (§5). [V] |
| PLAN §4 | `signalsmith-stretch` (MIT) en el stack de medios | **No está en ningún `Cargo.toml`** del workspace (búsqueda en todos los manifiestos). El warp es propio. [V] |

---

## 2. EQ y filtros

### 2.1 Qué usamos

| Pieza | Algoritmo | Dónde |
|---|---|---|
| Bandas estáticas (campana, shelves, pasa-banda) en modo Matched | Biquad de Vicanek: polos por invarianza al impulso y numerador ajustado en DC, Nyquist y `ω0`; diseño en `f64`, ejecución en `f32` TDF-II | `crates/dsp/src/biquad.rs:93-168, 264-289`; uso en `crates/stock/src/fx/eq/mod.rs:437` [V] |
| Bandas dinámicas, cortes 6–96 dB/oct, notch, tilt, y todo en modo Classic | SVF TPT (Simper/Zavalishin), transformada bilineal con prewarp en `ω0` | `crates/dsp/src/svf.rs:64-84`; `eq/mod.rs:14-18, 458` [V] |
| Natural / Linear | FIR por muestreo en frecuencia del prototipo analógico, UPOLS de 256 | documentado en 12/eq-compressor §1.2 [V] |
| Suavizado | Coeficientes cada 16 muestras desde frecuencia/ganancia/Q suavizadas | `eq/mod.rs:262` (`CONTROL = 16`) [V] |
| Crossovers | LR4 sobre SVF, o FIR complementario de fase lineal | `crates/dsp/src/crossover.rs:1-7, 23, 125-160` [V] |
| Banco del vocoder | Biquads RBJ pasa-banda | `crates/stock/src/fx/vocoder.rs:304` [V] |

### 2.2 Cramping medido (48 kHz)

Error de magnitud frente al prototipo analógico. Campana de +12 dB, Q 1:

| `f0` | SVF / RBJ a 18 kHz | Matched a 18 kHz | Peor error 20 Hz–20 kHz, SVF | Peor error, Matched |
|---|---|---|---|---|
| 2 kHz | −0,15 dB | −0,04 dB | 0,17 dB | 0,06 dB |
| 8 kHz | −2,22 dB | −0,48 dB | 2,22 dB | 0,49 dB |
| 12 kHz | −4,21 dB | −0,71 dB | 4,51 dB | 0,73 dB |
| 16 kHz | −3,22 dB | −0,45 dB | 6,18 dB | 0,99 dB |

Corte Butterworth (pasa-bajos de 12 dB/oct) evaluado a 20 kHz: con `fc` = 12 kHz el SVF queda 13,5 dB por debajo del analógico, con `fc` = 16 kHz 8,2 dB por debajo. El shelf agudo de +6 dB en 12 kHz se desvía +0,69 dB a 20 kHz (Matched: +0,14 dB). [V, sonda `cramp`]

**Qué significa.** El modo Matched cumple lo que promete para las bandas estáticas. Pero en cuanto una campana pasa a dinámica, o el usuario elige Classic, o usa un corte agudo, el EQ vuelve a la curva bilineal: el "aire" a 12–16 kHz suena distinto de lo que dibuja la curva analógica de la vista. Y la vista dibuja el prototipo analógico (12/eq-compressor §3), así que **la curva y el audio no coinciden** en esos casos.

### 2.3 Qué hacen mejor las referencias

![Cytomic: lista de papers técnicos, con el SVF de integración trapezoidal y el SVF "matched at Nyquist" de Ivanstov; el autor declara el contenido en dominio público. https://cytomic.com/technical-papers/](img/01-cytomic-papers-svf.png)

![ZL Equalizer 2: seis estructuras de filtro (Minimum Phase, State Variable, Parallel, Matched Phase, Mixed Phase, Zero Phase) y "advanced de-cramping technique". https://zl-audio.github.io/plugins/zlequalizer2/manual/](img/01-zl-equalizer2-estructuras-filtro.png)

| Referencia | Licencia | Qué tiene que nos falta |
|---|---|---|
| Cytomic, *Technical Papers* | Dominio público declarado en la página [V] | Además del SVF trapezoidal que ya usamos, un **SVF de un polo con mezcla ajustada a Nyquist** (`matched-svf1-ivanstov.pdf`) y versiones con coeficientes calculados solo con senos para menos ruido. Es la pieza que juntaría "modulable" y "sin cramping". https://cytomic.com/technical-papers/ [V] |
| M. Vicanek, artículos | Papers (libre lectura) | Además de *Matched Second Order Digital Filters* (2016, ya implementado), *Matched Two-Pole Digital Shelving Filters*, revisado el 3-may y el 10-dic-2025, y *Fast Settling Filters* para seguidores de envolvente. https://www.vicanek.de/articles/2poleShelvingFits.pdf, https://vicanek.de/articles.htm [V] |
| ZL Equalizer 2 | AGPL-3.0 [V] | Estructura "State Variable" para automatizar, "Matched/Mixed/Zero Phase" para fase analógica o limpia, de-cramping explícito, 64 bits de proceso, hasta 24 bandas y pendientes hasta 96 dB/oct. https://zl-audio.github.io/plugins/zlequalizer2/manual/, https://github.com/ZL-Audio/ZLEqualizer [V] |
| LSP Plugins 1.2.37 (1-nov-2026) | LGPL-3.0 [S] | Filtros de fase mínima en Filter / Graphic EQ / Parametric EQ y **convolución de latencia cero** para reducir la latencia de los modos IIR. https://lsp-plug.in/?page=news [V] |
| chowdsp_filters | GPL-3.0 [V] | Butterworth, Chebyshev II y elípticos de orden alto, `ModFilterWrapper` (biquad convertido en SVF modulable), transformadas conformes incluida la *alpha transform*. https://github.com/Chowdhury-DSP/chowdsp_utils [V] |
| Signalsmith DSP | MIT [V] | Diseñador de biquads con la variante de Vicanek y recaída a "one-sided" en shelves. https://signalsmith-audio.co.uk/code/dsp/html/group___filters.html [V] |

![Martin Vicanek: índice de artículos (matched biquads, shelving de 2 polos, Fast Settling Filters, reverb híbrida). https://vicanek.de/articles.htm](img/01-vicanek-articulos-matched.png)

### 2.4 Conclusión EQ/filtros

Lo único urgente es dar a las bandas dinámicas y a los cortes el mismo trato que a las estáticas: o un SVF ajustado a Nyquist (Cytomic/Ivanstov, dominio público), o el diseño de Vicanek para los cortes estáticos (el pasa-bajos y el pasa-altos ajustados ya existen en `biquad.rs:101-118` y no se usan en el EQ). El estado TDF-II en `f32` (`biquad.rs:266-269`) es suficiente para bandas de audio; las de muy baja frecuencia ya tienen `Biquad64` (`biquad.rs:291-327`).

---

## 3. Dinámica y limitador true-peak

### 3.1 Qué usamos

| Dispositivo | Algoritmo | Dónde |
|---|---|---|
| `fx.compressor` | Computador de ganancia log con rodilla cuadrática (Giannoulis, Massberg y Reiss 2012), detectores decoupled/branching, feedback con `α ≤ 1/R`, lookahead, SC con HPF/campana/LPF. Sin sobremuestreo. | `crates/stock/src/fx/compressor.rs:1-45` [V] |
| `fx.buscomp`, `fx.drumbus` | Mismo computador; soft clip `tanh` ADAA1 a 2× | `buscomp.rs:274`, `drumbus.rs:449` [V] |
| `fx.mbdyn` | LR4 o FIR lineal, compresión hacia abajo y hacia arriba por banda | `mbdyn.rs:1-45` [V] |
| `fx.limiter` | Ganancia necesaria por muestra → mínimo deslizante (Lemire) → doble release → cascada de cajas cuyo soporte es el ataque; TP por interpolador 4× (64 taps, Kaiser β=7, corte 0,92·Nyquist); sobremuestreo opcional 2×/4×; recorte final al techo | `crates/dsp/src/dynamics.rs:4-64, 104-190`; `limiter.rs:1-23, 83, 95-96, 700-701` [V] |

### 3.2 Mediciones

**Detector true-peak** (`TruePeak`, seno de amplitud 1, peor de 64 fases): [V, sonda `tp`]

| Frecuencia | Lo que el detector deja sin ver | Lo que deja un medidor de muestra |
|---|---|---|
| 1–10 kHz | ≤ 0,006 dB | ≤ 0,075 dB |
| 12 kHz (fs/4) | 0,170 dB (el peor caso geométrico de 4×: 1 − cos(π/16)) | 3,01 dB |
| 15 kHz | 0,012 dB | 0,169 dB |
| 18 kHz | **0,492 dB** | 0,688 dB |
| 20 kHz | **2,110 dB** | 0,301 dB |

A 20 kHz el detector lee *peor* que un simple medidor de muestras: el núcleo de 16 taps por fase con corte en 0,92 de Nyquist atenúa la parte alta (`dynamics.rs:143-145`).

**Limitador, techo −1 dB, True Peak ON**, pico real de salida medido con una referencia de 32× por FFT (zona central de 16 384 muestras): [V, sonda `lim`]

| Señal | 1× (por defecto) | 2× | 4× |
|---|---|---|---|
| Seno a fs/4 con fase de 45°, +6 dBFS | −0,85 dBTP (**+0,15 dB sobre el techo**) | −0,99 | −1,02 |
| Seno de 11 kHz, +6 dBFS | −1,02 | −1,02 | −1,03 |
| Diente de sierra de 3 kHz ya recortado, +12 dB | −0,65 dBTP (**+0,35 dB**) | −2,35 | −2,38 |
| Latencia | 87 muestras | 123 | 120 |

El valor por defecto del parámetro es 1× (`limiter.rs:96`, `OS = ["1x","2x","4x"]`, `limiter.rs:77`) con True Peak activado (`limiter.rs:95`). El margen interno es de solo 0,02 dB (`limiter.rs:83`). Es decir, la combinación por defecto promete "−1 dBTP" y no lo garantiza con material brillante y ya clipeado, que es justo lo que llega al master en música electrónica.

### 3.3 Qué hacen mejor las referencias

![x42-dpl: limitador con detección true-peak por clave sobremuestreada 4× y latencia de 1,2 ms (64 muestras a 48 kHz). https://x42-plugins.com/x42/x42-limiter](img/01-x42-dpl-true-peak.png)

![ZL Compressor: sobremuestreo hasta 8× con latencias publicadas de 64, 72 y 76 muestras a 48 kHz, clipper de salida y lookahead. https://zl-audio.github.io/plugins/zlcompressor/manual/](img/01-zl-compressor-oversampling-latencia.png)

| Referencia | Licencia | Qué hace |
|---|---|---|
| x42-dpl (Robin Gareus, basado en DPL-1 de Fons Adriaensen) | GPL-2 [S] | Latencia fija de 1,2 ms, clave sobremuestreada 4× solo en el detector, release con *hold* automático para graves (evita seguir la forma de onda). https://x42-plugins.com/x42/x42-limiter [V]. Sobre zipper: el umbral no se suaviza a propósito, "no sample must exceed the threshold, ever". https://github.com/x42/dpl.lv2/issues/3 [V] |
| LSP Limiter | LGPL-3.0 [S] | Sobremuestreo Lanczos 2×, 3×, 4×, 6× y 8×, solo de la cadena lateral ("Half") o de todo ("Full"); parches de reducción con formas Hermite/exp/lineal y cuatro envolventes; etapa ALR previa. https://lsp-plug.in/plugins/lv2/limiter_stereo [V] |
| ZL Compressor | AGPL-3.0 [V] | Hasta 8× de sobremuestreo, clipper posterior, lookahead, EQ de 8 bandas en el sidechain. https://zl-audio.github.io/plugins/zlcompressor/manual/ [V] |
| chowdsp_compressor | GPL-3.0 [V] | Detector y computador multimodales (solo estudio). https://github.com/Chowdhury-DSP/chowdsp_utils [V] |
| FabFilter Pro-L 2 | Comercial | Hasta 32× de sobremuestreo de fase lineal (referencia de mercado, no de código). https://www.fabfilter.com/news/1512462600/fabfilter-releases-fabfilter-pro-l-2-true-peak-limiter-plug-in [V] |
| ITU-R BS.1770-5 (11/2023), Anexo 2 | Norma | Método de referencia de true-peak por sobremuestreo; el anexo permite "un método que dé resultados similares o superiores". https://www.itu.int/rec/R-REC-BS.1770-5-202311-I [V]. La tabla de error máximo por factor la cito de memoria: [S]. |

![LSP Plugins 1.2.37: filtros de fase mínima, convolución de latencia cero para modos IIR, ventanas Kaiser-Bessel derivadas en el análisis espectral. https://lsp-plug.in/?page=news](img/01-lsp-plugins-1-2-37-notas.png)

### 3.4 Conclusión dinámica

El diseño del limitador es sólido; el hueco está en el detector. Dos arreglos baratos: un interpolador de true-peak más largo y con más fases (8× y 24–32 taps por fase, corte ≥ 0,95) para el modo 1× y para la etapa de seguridad, y que True Peak active por defecto 2×. El compresor no necesita sobremuestreo para la detección, pero sí lo ofrecen ZL y FabFilter para el *clipper* posterior: el nuestro ya existe en buscomp/drumbus a 2×.

---

## 4. Reverb

### 4.1 Qué usamos

FDN de 8 o 16 líneas con mezcla Walsh–Hadamard rápida (`crates/stock/src/fx/reverb.rs:154`), filtros de absorción por línea con RT60 exacto en graves/medios/agudos, lecturas moduladas con núcleo sinc de 8 taps (`reverb.rs:255, 300`), placa de Dattorro, nueve voicings, colores 70s/80s, ducking y shimmer. La calidad ya está medida en 12/reverb-delay ("Measured quality"): desviación espectral de la cola cerca del límite gaussiano, densidad de ecos y coherencia de la modulación. No la repito.

El **shimmer** es un desplazador de tono de dos granos de 80 ms con interpolación **lineal** (`crates/stock/src/fx/reverb/shimmer.rs:18, 59-60`). Para +12 st eso es remuestrear al doble con un interpolador lineal: imágenes y aliasing, que la difusión del tanque disimula pero que se realimentan en cada vuelta. [V código / S audibilidad]

### 4.2 Qué hacen mejor las referencias

![Signalsmith, "Let's Write A Reverb" (ADC21): FDN multicanal, Householder para 8 o más canales, difusor multicanal; el código de ejemplo es MIT. https://signalsmith-audio.co.uk/writing/2021/lets-write-a-reverb/](img/01-signalsmith-reverb-fdn.png)

![Dragonfly Reverb: Hall, Room, Plate y Early Reflections, construidas sobre Freeverb3 (GPL-3). https://michaelwillis.github.io/dragonfly-reverb/](img/01-dragonfly-reverb-familia.png)

| Referencia | Licencia | Qué hace mejor o distinto |
|---|---|---|
| Signalsmith, *Let's Write A Reverb* | Código de ejemplo MIT [V repo] | Diseño "sin números mágicos": difusor multicanal (varias etapas de retardo + Hadamard + inversión de signo) antes del FDN, Householder dentro del lazo. Nuestro diseño ya es FDN+Hadamard; lo útil es el **difusor multicanal** como sustituto de las cadenas de all-pass de Schroeder, que Valhalla asocia a artefactos metálicos. https://signalsmith-audio.co.uk/writing/2021/lets-write-a-reverb/, https://github.com/Signalsmith-Audio/reverb-example-code [V] |
| Valhalla DSP, blog | Artículos (producto comercial) | "Diffusion, allpass delays, and metallic artifacts" y "Shimmer: modulation, auto-correlation, and decorrelation": por qué el shimmer en un lazo con reverb modulada no encuentra buenos puntos de empalme y ensancha las bandas laterales. https://valhalladsp.com/2011/01/21/reverbs-diffusion-allpass-delays-and-metallic-artifacts/, https://valhalladsp.com/2010/05/12/shimmer-modulation-auto-correlation-and-decorrelation/ [V por lectura del texto; la página bloquea la captura sin navegador] |
| Dragonfly Reverb / Freeverb3 | GPL-3.0 / GPL-2 [V] | Cuatro reverbs separadas (sala, habitación, placa, reflexiones tempranas con tipos de espacio). Interesa el **módulo de reflexiones tempranas con patrones de salas reales**. https://github.com/michaelwillis/dragonfly-reverb, https://github.com/gitGNU/gnu_freeverb3 [V] |
| Airwindows kCathedral4/5 (2025) | MIT [V] | Reverbs dedicadas por espacio; kCathedral5 permite situar al oyente en el espacio. https://www.airwindows.com/kcathedral5/, https://synthanatomy.com/2026/03/airwindows-consolidated.html [V] |
| Dal Santo et al., *Shimmer Reverberation with Nonlinear Feedback Delay Networks*, DAFx26 (mejor paper) | Paper | Cinco formas de meter la no linealidad o el desplazamiento de tono **dentro** del FDN. https://dafx.de/paper-archive/2026/papers/DAFx26_paper_05.pdf, https://www.aalto.fi/en/news/shimmering-success-aalto-acoustics-lab-team-wins-best-paper-at-dafx-2026 [V] |
| M. Vicanek, *A New Hybrid Reverb Algorithm* | Paper | Colas de alta calidad con poco coste. https://vicanek.de/articles.htm [V título] |

### 4.3 Conclusión reverb

No hay un hueco algorítmico grande. Dos mejoras concretas: el shimmer con el mismo `SincKernel` que el tanque (o con el desplazador de §5 una vez arreglado) y probar el difusor multicanal de Signalsmith delante del FDN en los voicings Hall/Cloud. El paper de DAFx26 es la referencia natural si se quiere un shimmer que no dependa de un pitch shifter externo al lazo.

---

## 5. Delay y modulación

### 5.1 Qué usamos

| Dispositivo | Lectura del retardo / algoritmo | Dónde |
|---|---|---|
| `fx.delay` | Sinc de 8 taps, 256 fases, Kaiser β=7 (`crates/dsp/src/delay.rs:127-131, 197-212`); glide de pendiente limitada; cinta (wow/flutter) y BBD de Holters y Parker | `delay.rs:127, 450, 501-502` [V] |
| `fx.chorus` (chorus, flanger) | **Hermite cúbica** | `chorus.rs:218, 232` [V] |
| `fx.chorus` (phaser) | 2–12 all-pass TPT de primer orden con realimentación | `chorus.rs:69-70` [V] |
| `fx.freqshift` | Hilbert IIR de Niemitalo (dos cadenas de 4 all-pass) + oscilador en cuadratura | `freqshift.rs:49-52`; coeficientes idénticos a https://yehar.com/blog/?p=368 [V] |
| `fx.pitchshift` | Dos cabezales en una línea de retardo con ventana Hann y alineación por correlación (WSOLA en tiempo real); **Hermite** | `pitchshift.rs:1-21, 44-54, 268` [V] |
| `fx.vocoder` | Banco de 4–40 biquads RBJ pasa-banda | `vocoder.rs:304` [V] |
| `fx.stutter` | Hermite de 4 puntos | `stutter.rs` (doc del módulo) [V] |

12/reverb-delay ya midió lo que cuesta Hermite al mover el tiempo: imágenes de −32 dB frente a −73 dB con sinc. Chorus y flanger mueven el tiempo *siempre*, así que son justo los que más se beneficiarían, y siguen en Hermite.

**Pitch Shifter, medido**: +12 st sobre un seno de 13 kHz deja **22 kHz a −1,1 dB** (aliasing total); sobre 5 kHz da 10 kHz limpio. Latencia por defecto: **3268 muestras (68 ms)**, porque se informa la del grano máximo de 120 ms para que automatizar el grano no mueva el PDC (`pitchshift.rs:48-54, 222`). [V, sondas `pshift` y `latency`]

### 5.2 Qué hacen mejor las referencias

| Referencia | Licencia | Qué hace |
|---|---|---|
| Signalsmith DSP, *delay* | MIT [V] | Lectores de retardo intercambiables (nearest, lineal, cúbico, sinc de Kaiser) y sobremuestreador FIR 2× para bloques. https://signalsmith-audio.co.uk/code/dsp/html/group___delay.html, https://signalsmith-audio.co.uk/code/dsp/html/structsignalsmith_1_1rates_1_1_oversampler2x_f_i_r.html [V] |
| chowdsp_dsp_utils | GPL-3.0 [V] | `DelayLine` con más interpolaciones y un `PitchShifter` de dos lectores, el mismo esquema que el nuestro. https://github.com/Chowdhury-DSP/chowdsp_utils [V] |
| ChowMatrix | BSD-3 (ya citado en 12) | BBD por tap. Nada nuevo frente a 12. |
| DAFx26 paper 45, *A Clipping Prevention Method for All-Pass Digital Filters with Time-Varying Coefficients* | Paper | Justo el caso del phaser con realimentación y coeficientes modulados. https://www.dafx.de/paper-archive/2026/papers/DAFx26_paper_45.pdf [V título; contenido no leído: S] |
| Valhalla, *ValhallaDelay: The Diffusion Section* | Artículo | Difusión dentro del lazo con modulación proporcional al tamaño. https://valhalladsp.com/2019/06/13/valhalladelay-the-diffusion-section/ [V resumen] |

### 5.3 Conclusión delay/modulación

El delay está a nivel de referencia. Lo pendiente es coherencia: llevar el `SincKernel` a chorus, flanger, Pitch Shifter y shimmer (son 8 taps por lectura; el coste por voz es bajo) y, en el Pitch Shifter, filtrar antes de leer más rápido que la tasa (para `r > 1`, paso bajo a `0,45·fs/r` en la escritura de la línea, o núcleo sinc con corte escalado `1/r`).

---

## 6. Saturación, clipping y antialiasing

### 6.1 Qué usamos

| Pieza | Algoritmo | Dónde |
|---|---|---|
| Waveshapers | ADAA1 en `f64`, recaída al punto medio si `|Δx| < 1e-5` | `crates/dsp/src/shaper.rs:1-118` [V] |
| Sobremuestreo | Semibanda IIR polifásica (Valenzuela–Constantinides; coeficientes elípticos en forma cerrada como HIIR de L. de Soras): etapa 1 de 10 coeficientes y 2 % de transición, etapa 2 de 4; **máximo 4×** | `crates/dsp/src/oversample.rs:1-9, 184-199` [V]; licencia de HIIR (WTFPL): [S] |
| `fx.saturator` | 9 estilos ADAA1 dentro de 1×/2×/4×, multibanda LR4 | `saturator.rs:1-20, 216, 255` [V] |
| `fx.clipper` | `tanh` o recorte duro, ADAA1 solo a 2×/4×, **recorte final a la tasa base** | `clipper.rs:131, 153, 210-227, 236` [V] |
| `fx.amp`, `fx.tape`, `fx.console` | WDF con ω de Wright, Jiles–Atherton RK2, par sin/asin; a 2×/4× | 12/saturation [V] |
| `fx.filter` | `tanh` ADAA1 *antes* de un SVF lineal, a 1× | `filter.rs:245-248` [V] |

### 6.2 El defecto del Clipper, medido

Seno de 4987 Hz a −6 dBFS, Drive +12 dB, techo 0 dB. Energía no armónica (aliasing) relativa a los armónicos: [V, sonda `clip`]

| Curva, factor | Salida real | Antes del recorte final (reconstruido con *Delta*) |
|---|---|---|
| Soft 1× | −28,3 dB | −28,3 dB |
| Soft 2× | −33,7 dB | **−97,5 dB** |
| Soft 4× | −32,4 dB | **−98,5 dB** |
| Hard 1× | −25,5 dB | −25,5 dB |
| Hard 2× | −26,7 dB | **−53,8 dB** |
| Hard 4× | −26,0 dB | **−80,9 dB** |

La causa: tras diezmar, el filtro de semibanda devuelve un pico ligeramente por encima del techo (Gibbs), y `wet.clamp(-ceil, ceil)` (`clipper.rs:236`) lo recorta a la tasa base, que es exactamente el recorte duro sin antialias que el sobremuestreo quería evitar. El 1× tampoco tiene ADAA, a propósito, para mantener latencia cero (`clipper.rs:5-11`).

### 6.3 Qué hacen mejor las referencias

![Airwindows ClipOnly2: recorte duro que deja intactas las muestras no recortadas e interpola la entrada y salida del recorte; 1 muestra de latencia; MIT. https://www.airwindows.com/cliponly2/](img/01-airwindows-cliponly2.png)

![DAFx25: "Simplifying Antiderivative Antialiasing with Lookup Table Integration" (Gabrielli y Squartini): ADAA para curvas sin antiderivada cerrada. https://www.dafx.de/paper-archive/details/RwSEHYmiyYoDh3yJkkIiWw](img/01-dafx25-adaa-lut.png)

| Referencia | Licencia | Qué hace mejor |
|---|---|---|
| Airwindows ClipOnly2 | MIT [V] | Clip de seguridad a 1×: deja pasar intactas todas las muestras que no recortan y sintetiza una entrada y salida suaves interpolando entre la última muestra no recortada y la recortada; techo fijo en −0,2 dB; diseñado para cualquier tasa ("el espacio de una muestra a 44,1 kHz"). Es la respuesta directa a "1× sin latencia y sin brillo áspero". https://www.airwindows.com/cliponly2/, https://github.com/airwindows/airwindows (LICENSE MIT) [V] |
| Esqueda, Bilbao y Välimäki, *Aliasing Reduction in Clipped Signals*, IEEE TSP 64(20), 2016 | Paper | polyBLAMP: corrección de dos o cuatro muestras alrededor del punto de recorte; menos caro que sobremuestrear. https://dl.acm.org/doi/10.1109/TSP.2016.2585091, https://research.spa.aalto.fi/publications/papers/ieee-tsp-2016-clipping/readme.txt [V] |
| Gabrielli y Squartini, DAFx25 | Paper | ADAA con la antiderivada integrada numéricamente en una tabla: sirve para curvas medidas o sin forma cerrada (nuestras curvas de cinta, consola o amplificador). https://dafx.de/paper-archive/2025/DAFx25_paper_30.pdf [V] |
| Mikkonen y Werner (Soundtoys), DAFx25 | Paper | ADAA extendido a redes recurrentes (GRU/LSTM): relevante solo si algún día hay modelado neuronal. https://www.dafx.de/paper-archive/2025/DAFx25_paper_72.pdf [V] |
| *Interpolation Filters for Antiderivative Antialiasing*, DAFx24 | Paper | Compensa el medio retardo y la caída de agudos de ADAA1, el problema que 12/saturation ya anota ("−3 dB a fs/4 en la zona lineal"). https://dafx.de/paper-archive/2024/papers/DAFx24_paper_33.pdf [V título; detalle S] |
| chowdsp_waveshapers | GPL-3.0 [V] | Clippers duro, tanh y polinómico con ADAA, rectificador, wavefolders West Coast y Wave Multiplier. https://github.com/Chowdhury-DSP/chowdsp_utils [V] |
| ZL Compressor / FabFilter / LSP | AGPL / comercial / LGPL | 8× (ZL), 32× (FabFilter), hasta 8× Lanczos (LSP); nosotros 4× máximo. Ver §3.3. [V] |
| Signalsmith, *Oversampler2xFIR* | MIT [V] | Sobremuestreo FIR de fase lineal por bloques, con latencia y error de remuestreo documentados. Alternativa al IIR cuando importa la fase (clipper de mastering). https://signalsmith-audio.co.uk/code/dsp/html/structsignalsmith_1_1rates_1_1_oversampler2x_f_i_r.html [V] |

![chowdsp_utils: licencia por módulo; los módulos DSP (filtros, EQ, compresor, waveshapers, reverb) son GPL-3, solo core/math/simd son BSD. https://github.com/Chowdhury-DSP/chowdsp_utils](img/01-chowdsp-utils-licencias-modulos.png)

### 6.4 Conclusión saturación

La saturación es buena y está bien medida en 12. El arreglo del Clipper es trivial (hacer la protección de techo a la tasa alta, antes de diezmar y con un margen que cubra el rizado del filtro, o con un ClipOnly-style a la tasa base), y el 1× ganaría mucho con el enfoque de ClipOnly2 o con polyBLAMP. Para `fx.filter`, el "drive" de hoy es una saturación previa, no un filtro saturado: las referencias de VA (Cytomic, *From Circuit To Code*, ADC 2020, enlazado en la página de Cytomic) ponen la no linealidad en el lazo.

---

## 7. Pitch shift y time stretch del warp

### 7.1 Qué usamos (`crates/dsp/src/warp.rs`)

| Modo | Algoritmo | Detalles con riesgo |
|---|---|---|
| Beats | Rebanadas por transitorio (flujo de energía + HF, mediana, refractario 45 ms, adelanto de 2 ms); al alargar repite la cola en ping-pong con caída exponencial | `warp.rs:83-152, 156-234`; ping-pong en `:202-209` [V] |
| Tones | WSOLA estéreo enlazado, ventana 1536, búsqueda ±320, correlación gruesa cada 4 muestras sobre una muestra de cada dos | `warp.rs:238-344, 276-296, 689` [V] |
| Texture | Granular estocástico | `warp.rs:358` [V] |
| Complex | Vocoder de fase: FFT 2048, salto de síntesis 512, *phase locking* por región de influencia del pico más cercano (Laroche y Dolson), reinicio de fase si la energía del frame sube ×2,8 **y supera un umbral absoluto 0,5** | `warp.rs:464-640, 470, 478, 535, 544-575` [V] |
| Complex Pro | Igual + envolvente cepstral (lifter 32) para conservar formantes al transponer | `warp.rs:580-605, 584` [V] |
| Re-Pitch | Hermite | `warp.rs:664-667` [V] |
| Transposición en todos | Se estira por `pitch_ratio` y luego se **remuestrea con Hermite sin filtro** | `warp.rs:56-69, 634-636, 694, 703` [V] |

El render es offline y en un worker (§1). En tiempo real, con rampas de tempo, se usan `sample_warped_beats` y `sample_warped_grain` (`warp.rs:713-806`).

### 7.2 Mediciones frente a Signalsmith Stretch 1.3.1 (MIT, vía la crate `signalsmith-stretch` 0.1.3)

Todas a 48 kHz, mono, `preset_default` de Signalsmith. [V, sondas `warpalias` y `stretch`]

| Prueba | Complex | Complex Pro | Tones | Texture | Beats | Signalsmith |
|---|---|---|---|---|---|---|
| +12 st sobre 13 kHz (debe desaparecer): residuo | **0,0 dB** (todo aliasado a 22 kHz) | −12,0 dB | **0,0 dB** | — | — | **−75,4 dB** |
| Estirar ×1,25 un acorde estacionario de 4 notas: energía fuera de los parciales | −17,3 dB | −17,3 dB | −2,5 dB | +2,4 dB | +1,7 dB | **−29,5 dB** |
| Tren de clics ×1,25: energía > 2 ms antes del pico (pre-eco) | −13,5 dB | −13,5 dB | −20,1 dB | — | sin pre-eco | −18,4 dB |
| Ídem: energía > 10 ms después del pico | −34,4 dB | −34,4 dB | sin cola | — | sin cola | −25,5 dB |
| Ídem: dispersión de la posición del clic (máx − mín de 8 clics) | **160 muestras (3,3 ms)** | 160 | 105 (2,2 ms) | — | 0 (desfase fijo −84) | 10 |

Re-Pitch también da 0,0 dB en la prueba de aliasing: es remuestreo puro con Hermite. Beats y Texture no están pensados para material tonal; sus cifras en el acorde son las esperables. La latencia de Signalsmith con `preset_default` es de 2880 muestras de entrada y 2880 de salida, irrelevante en render offline.

**Lectura.** (1) La transposición hacia arriba es inutilizable por encima de unos `fs/(2·ratio)` (12 kHz para +12 st): hi-hats y platillos transpuestos suenan con tonos inarmónicos. (2) El vocoder de fase de tamaño fijo (43 ms de ventana) y el reinicio de fase por umbral absoluto de energía hacen que los ataques bailen ±1,6 ms y lleven pre-eco: en un loop de batería en modo Complex se oye como "flam". (3) Signalsmith deja más cola tras el transitorio (−25,5 dB) pero no pre-eco ni jitter, que es lo que el oído castiga.

### 7.3 Qué hacen mejor las referencias

![Signalsmith Stretch: versiones 1.0.0 (dic-2024), 1.1.0 (ene-2025), 1.2.0 y 1.3.0 (abr-2025) y 1.4.0 (25-sep-2026). https://github.com/Signalsmith-Audio/signalsmith-stretch/tags](img/01-signalsmith-stretch-versiones.png)

![Signalsmith Stretch: página del proyecto, "extension of the final approach from our ADC22 presentation Four Ways To Write A Pitch-Shifter". https://signalsmith-audio.co.uk/code/stretch/](img/01-signalsmith-stretch-proyecto.png)

![Bungee: vocoder de fase adaptativo, cualquier relación incluida velocidad cero y negativa, misma API que Bungee Pro (comercial). https://bungee.parabolaresearch.com/](img/01-bungee-vocoder-adaptativo.png)

| Referencia | Licencia | Qué hace |
|---|---|---|
| Signalsmith Stretch | MIT [V] | Biblioteca de cabecera C++11; transposición de varias octavas, suena mejor entre ×0,75 y ×1,5 de estiramiento; *tonality limit* (mapa de frecuencias no lineal), mapa de frecuencias arbitrario, **compensación de formantes** (`setFormantFactor`, `setFormantBase`), `seek()`/`flush()` para estirar exacto un archivo, `splitComputation` para repartir el coste. https://github.com/Signalsmith-Audio/signalsmith-stretch, README en https://raw.githubusercontent.com/Signalsmith-Audio/signalsmith-stretch/main/README.md [V]. Crate Rust `signalsmith-stretch` 0.1.3 (MIT, incluye la versión 1.3.1 del C++; compila con `cc` y `bindgen`) [V en el registro local]. |
| Bungee | MPL-2.0 [V LICENSE] (admitida en `deny.toml`) | Vocoder de fase adaptativo, unos 100 granos por segundo, API granular con posición, velocidad y tono por grano (velocidad cero, negativa, scrub) o API de flujo; v2.4.39 (30-oct-2026). https://github.com/bungee-audio-stretch/bungee, https://github.com/bungee-audio-stretch/bungee/releases, https://bungee.parabolaresearch.com/ [V] |
| Bungee Pro | Comercial | Transitorios y timbre mejores según el fabricante; SDK nativo y web. https://bungee.parabolaresearch.com/ [V] |
| Rubber Band R3 ("Finer") | GPL-2 o comercial | El motor R3 de Rubber Band 3 es de mayor calidad y más caro en CPU. https://breakfastquay.com/rubberband/code-doc/classRubberBand_1_1RubberBandStretcher.html, https://thebreakfastpost.com/2022/09/30/performance-improvements-in-rubber-band-library/ [V]. Ya excluido en `PLAN.md:429` salvo licencia comercial. |
| Průša et al., *Phase Vocoder Done Right* / RTPGHI | Paper | Reconstrucción de fase por integración del gradiente (PGHI) en tiempo real: elimina la "fasiness" sin bloqueo por picos. https://www.alphaxiv.org/abs/2202.07382, https://ltfat.org/notes/ltfatnote043.pdf [V] |
| Röbel, *Transient detection and preservation in the phase vocoder*, ICMC 2003 | Paper | Detecta transitorios por pico espectral (centro de gravedad del retardo de grupo) y reinicializa la fase solo de esos picos, en el frame correcto: justo lo que nos falta frente al umbral de energía. https://nagasm.org/ASL/icmc2003/closed/CR1074.PDF [V] |
| Laroche y Dolson, *Improved phase vocoder time-scale modification of audio*, IEEE TSAP 1999 | Paper | El *phase locking* que ya usamos. https://ui.adsabs.harvard.edu/abs/1999ITSAP...7..323L/abstract [V] |
| Driedger y Müller, *TSM Toolbox*, DAFx 2014 | Paper + código MATLAB | Referencia de la familia WSOLA/PV/HPSS-TSM (separar armónico y percusivo y estirar cada uno con su método). https://www.audiolabs-erlangen.de/content/resources/MIR/TSMtoolbox/2014_DriedgerMueller_TSM-Toolbox_DAFX.pdf [V] |

### 7.4 Conclusión warp

Dos decisiones, en este orden. Primero, arreglar la transposición (filtro antialias en `resample_channels`, o transponer en el dominio espectral como hace Signalsmith con su mapa de frecuencias): es un cambio local, de días. Segundo, para Complex y Complex Pro, usar la crate `signalsmith-stretch` (MIT, prevista en `PLAN.md:136`) detrás de la misma interfaz `warp_audio`, y quedarnos con Beats y Tones propios. Si se prefiere no depender de C++, la alternativa es reescribir el vocoder con varias resoluciones y la detección de Röbel; es trabajo L y no garantiza alcanzar a Signalsmith.

---

## 8. Novedades 2025–2026 en los proyectos de referencia

| Proyecto | Novedad | Fuente |
|---|---|---|
| Signalsmith Stretch | 1.1.0 (29-ene-2025), 1.2.0 (16-abr-2025), 1.3.0 (18-abr-2025), 1.4.0 (25-sep-2026); CMake y retirada de la copia de la biblioteca DSP (feb-2025) | https://github.com/Signalsmith-Audio/signalsmith-stretch/tags [V] |
| Bungee | Series v2.4.x durante 2026, último v2.4.39 (30-oct-2026); soporte de 384 kHz en v2.4.23 | https://github.com/bungee-audio-stretch/bungee/releases [V] |
| LSP Plugins | 1.2.37 (1-nov-2026): filtros de fase mínima en EQ y Filter, pendientes de 6 y 18 dB/oct en crossovers, convolución de latencia cero para modos IIR, ventanas KBD; 1.2.35 (23-ago-2026) con builds oficiales de Windows | https://lsp-plug.in/?page=news [V] |
| ZL Audio | ZL Equalizer 2 (6 estructuras, de-cramping, 24 bandas) y ZL Compressor con commits en noviembre de 2026; el README del EQ declara desde octubre de 2026 asistencia de LLM en el desarrollo | https://github.com/ZL-Audio/ZLEqualizer, https://github.com/ZL-Audio/ZLCompressor [V] |
| Airwindows | kCathedral4/5 (2025), ConsoleX2, ConsoleH, ToTape9, TapeHack2, SmoothEQ3, PurestSaturation, VerbThic/VerbTiny; Consolidated supera 500 algoritmos (actualización de marzo de 2026); en noviembre de 2026, PunchyDeluxe2 y Weave | https://synthanatomy.com/2026/03/airwindows-consolidated.html, https://www.airwindows.com/cliponly2/ (barra lateral) [V] |
| chowdsp_utils | Último commit en la rama principal: 9-dic-2025; sin release nuevo visto | https://github.com/Chowdhury-DSP/chowdsp_utils [V] |
| Vicanek | *Matched Two-Pole Digital Shelving Filters*, revisado 3-may y 10-dic-2025 | https://www.vicanek.de/articles/2poleShelvingFits.pdf [V] |
| DAFx25 (Ancona) | ADAA por tabla (Gabrielli y Squartini); ADAA para RNN (Mikkonen y Werner) | https://dafx25.dii.univpm.it/detailed-program/ [V] |
| DAFx26 (MIT, 1–4 sep 2026) | Mejor paper: shimmer con FDN no lineal (Dal Santo et al.); prevención de recorte en all-pass variantes en el tiempo (paper 45); manipulación del retardo de grupo (paper 02) | https://dafx26.mit.edu/program/, https://www.aalto.fi/en/news/shimmering-success-aalto-acoustics-lab-team-wins-best-paper-at-dafx-2026 [V] |
| fundsp | 0.23.0, Apache-2.0 (en GitHub; crates.io la publica también como MIT [S]) | https://github.com/SamiPerttu/fundsp [V] |
| nih-plug | En modo mantenimiento; framework ISC, bindings VST3 GPL-3, plugins de `plugins/` "may be" GPL-3 | https://github.com/robbert-vdh/nih-plug [V] |
| Surge XT | 1.3 (dic-2023) añadió Bonsai y filtros Airwindows; no verifiqué novedades de efectos en 2025–2026 [S] | https://surge-synthesizer.github.io/changelog/ [V para 1.3] |

No reviso Vital (GPL-3, sin actividad que conste en las fuentes que leí) ni el módulo `juce_dsp` (AGPL-3 o comercial desde JUCE 8, https://juce.com/get-juce/ [V]) más allá de su licencia: no aportan un algoritmo de efectos que no esté ya cubierto por las referencias anteriores. [S]

---

## 9. Licencias de lo citado

| Recurso | Licencia | Uso permitido para nosotros |
|---|---|---|
| Signalsmith Stretch, Signalsmith DSP, ejemplo de reverb | MIT | Dependencia o adaptación con NOTICE |
| Crate `signalsmith-stretch` 0.1.3 | MIT | Dependencia (compila C++, necesita libclang para `bindgen`) |
| Bungee | MPL-2.0 | Dependencia (copyleft por archivo; admitida en `deny.toml`) |
| Airwindows | MIT | Adaptación con NOTICE |
| Cytomic technical papers | Dominio público declarado | Libre |
| Vicanek, DAFx, IEEE, ICMC, ITU | Papers | Implementación propia |
| fundsp | Apache-2.0 | Dependencia o estudio |
| nih-plug (framework y ejemplos) | ISC | Estudio; los plugins de `plugins/` pueden ser GPL-3 |
| chowdsp_utils | Módulos DSP GPL-3; core, math, simd BSD | Solo estudio para lo DSP |
| ZL Equalizer 2, ZL Compressor | AGPL-3.0 | Solo estudio |
| Dragonfly Reverb, Surge XT, Vital | GPL-3.0 | Solo estudio |
| Freeverb3 | GPL-2 | Solo estudio |
| LSP Plugins | LGPL-3.0 [S] | Solo estudio |
| x42-dpl | GPL-2 [S] | Solo estudio |
| JUCE 8 (`juce_dsp`) | AGPL-3.0 o comercial | Solo estudio |
| Rubber Band | GPL-2 o comercial | Excluido (`PLAN.md:429`) |
| Bungee Pro, FabFilter, Valhalla | Comerciales | Solo documentación pública |

---

## 10. Mejoras priorizadas

Esfuerzo: **S** = días, **M** = 1–2 semanas, **L** = más. El impacto se refiere a lo que oye o nota el usuario.

| # | Qué cambiar | Archivo | Algoritmo o referencia | Esfuerzo | Impacto | Riesgo de licencia |
|---|---|---|---|---|---|---|
| 1 | Quitar el recorte duro a la tasa base cuando el sobremuestreo está activo: proteger el techo a la tasa alta con un margen que cubra el rizado del diezmado, y en 1× sustituir el `clamp` por un recorte tipo ClipOnly2 (entrada y salida interpoladas) o polyBLAMP. Añadir un test de aliasing con 2×/4× que hoy fallaría (−26 dB frente a −54/−81 dB) | `crates/stock/src/fx/clipper.rs:210-236` | Airwindows ClipOnly2; Esqueda, Bilbao y Välimäki 2016 | S | Alto: es el clipper de mastering; hoy 2×/4× no sirven | Ninguno (idea; MIT si se adapta) |
| 2 | Filtro antialias al transponer: sustituir `resample_channels` (Hermite) por un remuestreador con paso bajo a `0,45·fs/ratio` (sinc enventanado polifásico o `rubato`, que ya está en el árbol), y lo mismo en el Pitch Shifter y el shimmer (núcleo sinc con corte `1/r` o prefiltro en la escritura) | `crates/dsp/src/warp.rs:56-69, 634-636, 694, 703`; `crates/stock/src/fx/pitchshift.rs:268`; `crates/stock/src/fx/reverb/shimmer.rs:59-60` | `SincKernel` propio (`crates/dsp/src/delay.rs:127-212`) con corte escalado | S | Alto: hoy +12 st deja el contenido de 12–24 kHz reflejado a nivel completo | Ninguno |
| 3 | Limitador: interpolador true-peak de 8 fases y 24–32 taps con corte ≥ 0,95, usado en el detector a 1× y en la etapa de seguridad; True Peak ON → 2× por defecto en presets nuevos; test que mida la salida con una referencia de 32× (como la sonda) en señales ISP | `crates/dsp/src/dynamics.rs:104-190`; `crates/stock/src/fx/limiter.rs:95-96` | BS.1770-5 Anexo 2; x42-dpl, LSP (Lanczos 8×) | S | Alto en entrega: hoy −1 dBTP puede salir a −0,65 dBTP | Ninguno |
| 4 | Complex y Complex Pro con Signalsmith Stretch detrás de `warp_audio` (offline, en el worker que ya existe), con `setFormantFactor` para Complex Pro; mantener Beats/Tones/Texture propios | `crates/dsp/src/warp.rs:706-707`; `crates/engine/src/sample_cache.rs:657` | Crate `signalsmith-stretch` (MIT, prevista en `PLAN.md:136`) | M | Alto: −12 dB de artefactos, sin jitter de ataques, sin aliasing al transponer | Bajo: MIT; añadir NOTICE; dependencia C++ (`cc`, `bindgen`) |
| 5 | EQ sin cramping en bandas dinámicas, cortes y modo Classic: SVF ajustado a Nyquist para lo que se modula, y biquads de Vicanek para cortes estáticos (ya existen en `biquad.rs:101-118`); que la curva de la vista use la respuesta real del filtro cuando no coincide con la analógica | `crates/dsp/src/svf.rs:64-84`; `crates/stock/src/fx/eq/mod.rs:437, 458` | Cytomic `matched-svf1-ivanstov.pdf`; Vicanek 2016 y 2025 | M | Medio-alto: hasta 4–6 dB de error a 12–18 kHz en campanas, 8–13 dB en cortes | Ninguno (dominio público / paper) |
| 6 | `SincKernel` en chorus y flanger (hoy Hermite) | `crates/stock/src/fx/chorus.rs:218, 232` | Igual que `fx.delay` | S | Medio: imágenes de −32 dB a ~−73 dB en modulación continua | Ninguno |
| 7 | Si se mantiene el vocoder propio (o como plan B de la #4): transitorios por pico espectral (Röbel) en vez de energía ×2,8 con umbral absoluto, y ventana adaptativa o varias resoluciones; reconstrucción de fase PGHI | `crates/dsp/src/warp.rs:470-578` | Röbel 2003; Průša (RTPGHI); Bungee (MPL-2.0) como lectura | L | Alto si no se hace la #4 | Ninguno (papers); MPL-2.0 si se usa Bungee |
| 8 | Sobremuestreador: añadir 8× y una opción FIR de fase lineal para Clipper y Limiter | `crates/dsp/src/oversample.rs:183-259` | Signalsmith `Oversampler2xFIR` (MIT); ZL/LSP/FabFilter como referencia de producto | M | Medio: mastering a 44,1 kHz | Ninguno (MIT si se adapta) |
| 9 | ADAA más allá de ADAA1: tabla integrada para curvas sin antiderivada (cinta, consola, amplificador) y filtro de interpolación que compense el medio retardo y la caída de agudos | `crates/dsp/src/shaper.rs`; `crates/stock/src/fx/saturator/*` | Gabrielli y Squartini, DAFx25; DAFx24 paper 33 | M | Medio: menos sobremuestreo para el mismo aliasing | Ninguno |
| 10 | Filtro con saturación en el lazo (SVF no lineal o ladder) para cumplir lo que promete el índice de 12 | `crates/stock/src/fx/filter.rs:245-248` | Cytomic *From Circuit To Code* (ADC 2020); Zavalishin | M | Medio: el filtro de sweeps de la música electrónica | Ninguno |
| 11 | Shimmer dentro del FDN y difusor multicanal delante del tanque | `crates/stock/src/fx/reverb.rs`, `reverb/shimmer.rs` | Dal Santo et al., DAFx26; Signalsmith *Let's Write A Reverb* | M | Bajo-medio | Ninguno (paper; MIT) |
| 12 | Phaser con realimentación: prevención de recorte en all-pass variantes | `crates/stock/src/fx/chorus.rs:69-70, 252` | DAFx26 paper 45 | S | Bajo | Ninguno |

---

## 11. Cómo reproducir las mediciones

Sonda: `~/.capy/work/fxprobe` (crate binaria propia que depende de `aitrack-dsp` y `aitrack-stock` por ruta; no está en el repo). Se compila con `CARGO_TARGET_DIR=$PWD/target cargo build --release --features ss` (la *feature* `ss` añade la crate `signalsmith-stretch`) y se ejecuta con `./target/release/fxprobe <cramp|tp|lim|clip|warpalias|stretch|latency|pshift>`. Resultados guardados en `results.txt`, `results2.txt` y `results3.txt` del mismo directorio.

- **cramp:** `SvfCoeffs::magnitude_db`, `BiquadCoeffs::{rbj,matched}` contra `analog_magnitude_db`.
- **tp:** `TruePeak` sobre senos de amplitud 1, 64 fases.
- **lim:** `fx.limiter` con techo −1 dB y TP activado, 2 s de señal, pico real por relleno de ceros en FFT a 32× en la zona central.
- **clip:** `fx.clipper`, salida real y salida reconstruida antes del recorte final (*Delta* + seco alineado).
- **warpalias / pshift:** seno de 13 kHz, +12 st, nivel residual y su reparto en 21–23 kHz.
- **stretch:** acorde 220/277/330/880 Hz estirado ×1,25 (energía fuera de ±6 bins de cada parcial, ventana Blackman-Harris de 65 536); tren de clics cada 0,5 s (reparto de energía alrededor del pico de cada clic y dispersión de su posición).
- **latency:** `latency()` de cada efecto tras `prepare(48 kHz, 128)`: eq, compressor, buscomp, multiband, gate, transient, reverb, delay, chorus, crusher, freqshift, vocoder, filter, stutter y shaper = 0; saturator, clipper, drumbus, tape, amp y console = 4; limiter = 87; mbdyn = 255; pitchshift = 3268. [V]
