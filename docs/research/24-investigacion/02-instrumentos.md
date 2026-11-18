# 24.02 · Instrumentos y efectos MIDI frente a los sintetizadores y samplers open source

Fecha: 17-nov-2026 · Rama `main` (commit `574c9e5`) · Alcance: algoritmos de `crates/stock/src/instrument` (wavetable, acid303, kick808, drum, sampler, sfz, granular, vault, synth_fx), primitivas de `crates/dsp/src` (osc, wavetable, envelope, lfo, svf, oversample) y efectos MIDI de `crates/stock/src/midi`. Investigación: no se tocó código de producción.

> **Convenciones.** `[V]` = verificado (leído en nuestro código con ruta:línea, o en la fuente externa con su URL). `[S]` = supuesto o inferencia mía. Cada repositorio externo lleva su licencia. Los proyectos GPL/LGPL/AGPL se citan solo para entender el algoritmo; la recomendación es siempre una implementación propia (regla de `CLAUDE.md` y `deny.toml`).

---

## 0. Resumen

Nuestros instrumentos están bien resueltos en lo que más se ve (arquitectura, tiempo real, matriz de modulación del wavetable, granular determinista, efectos MIDI deterministas por posición) y flojos en tres capas de DSP que los sintes open source de referencia tratan con más cuidado:

1. **Los filtros "analógicos" no son el mismo filtro tres veces.** Hay tres escaleras distintas y ninguna es un ZDF no lineal sobremuestreado de verdad: el ladder del wavetable resuelve el lazo lineal y pone la `tanh` solo a la entrada, a 1× (`wavetable/filter.rs:229`); el `Cascade4` de Vault usa realimentación con un retardo de una muestra y un "2×" que repite la misma entrada y promedia (`vault/kit.rs:243-303`); y el `Diode18` del Acid 303 son tres polos de un polo con actualización explícita, realimentación retrasada y un "2×" sin filtro de diezmado (`acid303.rs:128-160`). El sobremuestreador polifásico bueno que ya tenemos (`dsp/src/oversample.rs:1-9`) **no lo usa ningún instrumento** (solo efectos) [V].
2. **El sampler no tiene antialiasing al transponer hacia arriba** ni la interpolación sinc que promete PLAN §5: lee con Hermite de 4 puntos (`instrument/sample.rs:78-84`) y avanza `rate_mul` muestras por muestra (`drum.rs:366,381`). Lo irónico es que el granular sí lo resuelve con una cadena de mips de media banda (`granular/source.rs:1-20`), y se puede reutilizar.
3. **El streaming de disco es frágil con polifonía**: una sola petición de página por zona, seis páginas en anillo, el WAV se reabre en cada página y un fallo devuelve silencio (`sample.rs:319,372,432`). sfizz (BSD-2) usa un modelo más simple y robusto (precarga + carga completa en segundo plano con contador de lectores).

Lo más rentable por orden: un módulo único de filtros VA en `aitrack-dsp` corriendo dentro del `Oversampler` existente, el filtro TB-303 rehecho siguiendo Open303 (MIT), reutilizar la cadena de mips del granular en sampler y drum, y rehacer el streaming al estilo sfizz. La lista completa con esfuerzo, impacto y riesgo de licencia está en §11.

---

## 1. Qué ha cambiado respecto a los informes anteriores

| Informe y afirmación | Estado hoy | Evidencia |
|---|---|---|
| 13 §1.5 y 18 §B.3: "Dispositivos MIDI: ausente; `REGISTRY` no registra ninguno" | **Resuelto.** Existen `midi.arp`, `chord`, `scale`, `velocity`, `random`, `echo`, `length`, `pitch` | `stock/src/midi/mod.rs:1-16` [V] |
| 18 §B.4 y 20 trabajo 4.12: "`instrument.granular`: ausente" | **Resuelto**, con fuente sample/live/built-in, mips antialias y determinismo por posición | `granular/mod.rs:1-55` [V] |
| 10 §3 "Gap for the coordinator": un `SetDeviceState` no llega a un dispositivo que suena | **Resuelto**: un estado nuevo reconstruye el procesador fuera del hilo RT | `engine/src/shared.rs:326-337`, `engine/src/tests/device_state.rs:1-3` [V] |
| PLAN §5: "Sampler + Drum Sampler: streaming + interpolación **sinc**" | **No cumplido**: Hermite de 4 puntos, sin filtro previo al transponer | `sample.rs:78-84` [V] |
| PLAN §5: "Sintetizador… PolyBLEP + wavetables band-limited por octava, **filtros ZDF**" | Parcial: SVF ZDF sí (`dsp/src/svf.rs:1-9`); el ladder solo resuelve sin retardo la parte lineal | `wavetable/filter.rs:229-245` [V] |
| 12 (acid-808-shaper-sfz): "`Diode18`: 2×-oversampled 18 dB/oct non-linear diode-ladder" | **Impreciso**: no es una escalera de diodos acoplada sino tres polos independientes con actualización explícita; el "2×" repite la entrada y no diezma | `acid303.rs:142-160` [V] |
| 11 §2.1: ladder de Vault "TPT… a 2× de sobremuestreo para que la resonancia afine" | Los polos son TPT, pero el lazo global va con una muestra de retardo (el propio código lo compensa con `1 + 0.5·g`) y el 2× es repetición + promedio | `vault/kit.rs:270-303` [V] |
| 22 §1.2: "el motor no usa `Note.slide`" | **Sigue igual**: `grep slide crates/engine/src` no encuentra consumidor; el Acid 303 desliza por solape de notas | `model/src/project.rs:768-770`, `acid303.rs:337-344` [V] |
| 13 §1.5 y 18 §B.4: MPE ausente | Sigue parcial: el evento tiene 5 dimensiones por nota, solo el wavetable lee 2 | §6 abajo |

---

## 2. Referencias estudiadas y su licencia

| Proyecto | Licencia | Estado verificado (nov-2026) | Para qué sirve aquí |
|---|---|---|---|
| Surge XT | GPL-3 [S: conocido, no releído] | 1.3.4 es la última estable; el changelog nightly dice "We are approaching version 1.4.0 release" (13-oct-2026) y trae editor de wavetables en Lua y Formula con 16 salidas [V https://surge-synthesizer.github.io/nightlychangelog/]. Milestone 1.4.0 al 96 % y 2.0 al 12 % [V https://github.com/surge-synthesizer/surge/milestones] | Catálogo de filtros, MSEG, osciladores, 2× fijo |
| Vital | GPL-3 (licencia comercial aparte) [V https://github.com/mtytel/vital] | "This repository is updated on a delay after binary releases" [V misma URL]; no verifiqué versión nueva en 2025-26 [S] | Oversampling 1×–8×, warps espectrales |
| Odin 2 | GPL-3+ [V https://librearts.org/2025/05/odin-2-4-0/] | 2.4.0 (rediseño de UI) y 2.4.1 [V https://github.com/TheWaveWarden/odin2/blob/master/change_log.md] | Diode ladder y K35 (Surge los adoptó) |
| Dexed / MSFA | Dexed GPL-3 [S]; **MSFA Apache-2.0** [V https://github.com/google/music-synthesizer-for-android/blob/master/COPYING] | Dexed v0.9.9 (16-nov-2025) y v1.0.1 (29-nov-2025) [V https://github.com/asb2m10/dexed/releases/tag/v1.0.1]; MSFA archivado [V repo] | Envolventes y escalado DX7 para FM6 |
| Helm | GPL-3 (informe 10) | Sin novedades buscadas | Ya cubierto en 10 |
| OB-Xd | 2.x GPL-3 (comunidad: https://github.com/NickCulbertson/OB-Xd-GPL) [V] | discoDSP vende **OB-Xd 4**, motor nuevo "modeled on the Oberheim OB-X8", precio de lanzamiento hasta el 31-dic-2026 [V https://www.discodsp.com/obxd/]; no consta que el 4 sea abierto [S] | Filtros SEM 12 dB / 24 dB (vía Surge) |
| Monique | **GPL-3 o MIT, a elegir** [V https://github.com/surge-synthesizer/monique-monosynth/blob/main/LICENSE] | — | Mono-synth con secuenciador y morph: se puede estudiar y adaptar con `NOTICE` |
| Cardinal / VCV Rack | Rack GPL-3+ [V https://vcvrack.com/manual/PluginLicensing]; Cardinal incluye módulos MIT/BSD/CC0 [S, hilo HN] | — | Mutable Instruments: **código STM32 MIT** [V https://github.com/pichenettes/eurorack] |
| sfizz | **BSD-2** [V https://github.com/sfztools/sfizz] | Última release 1.2.3 (14-ene-2024) y **repositorio archivado el 21-jun-2026** [V captura] | Interpoladores, FilePool, cobertura SFZ |
| Decent Sampler | Gratuito; no encontré código fuente [S: cerrado] | Documentación de formato 1.43.1 (dev) [V captura] | Comportamiento de grupos, mono, glide |
| Geonkick | GPL-3+ [V README] | 3.6.0 (3-jul-2025), etiqueta 3.7.0 (2-ene-2026), README en 3.9.1 [V https://quamplex.com/2025/07/03/geonkick-version-3-6-0-has-been-released/, https://gitlab.com/quamplex/geonkick] | Diseño de un sinte de percusión por capas |
| Open303 | **MIT** [V https://raw.githubusercontent.com/RobinSchmidt/Open303/main/License.txt] | Continuación en GitHub como CLAP [V https://github.com/RobinSchmidt/Open303] | Filtro TB-303, slide, acento |
| Ardour ACE | GPL-2 [S] | ACE Reasonable Synth: "very minimal synth… with no controls" [V https://manual.ardour.org/plugins-instruments/] | Nada que aprender en DSP |
| fundsp | **MIT o Apache-2.0** [V README §License] | Último commit 3-mar-2026 [V GitHub] | Oráculo de tests en Rust |
| nih-plug | ISC [V] | "currently in maintenance mode" [V https://github.com/robbert-vdh/nih-plug] | Sin cambios respecto a PLAN §5 |

![Repositorio de sfizz en GitHub con el aviso "archived by the owner on Jun 21, 2026", licencia BSD-2 y última release 1.2.3. https://github.com/sfztools/sfizz](img/02-sfizz-repo-archivado.png)

---

## 3. Osciladores y antialiasing

### 3.1 Qué tenemos

| Pieza | Algoritmo | Dónde |
|---|---|---|
| `BlepOsc` (VA de Vault, Acid, sub del wavetable) | PolyBLEP de 2 muestras para sierra/pulso, PolyBLAMP para triángulo (Välimäki-Huovilainen 2007; Esqueda-Välimäki-Bilbao 2016) | `dsp/src/osc.rs:1-34, 74-100` [V] |
| `MipTable` (wavetable de serie) | 10 niveles por octava (512→1 armónicos), cada nivel a 4× su armónico superior (mín. 256 muestras), lectura Hermite | `wavetable/table.rs:17-29, 176-199, 239` [V] |
| `dsp::Wavetable` (otro, en dsp) | Misma idea con 8× y lectura lineal | `dsp/src/wavetable.rs:17-27, 119-139` [V] |
| Warps del wavetable | Sync = multiplicar la fase, Bend, PWM, Mirror, Quantize, Remap, PM y RM del vecino; `warp_speed` escoge un nivel más fino | `wavetable/mod.rs:619-662, 770` [V] |
| Hard sync del Poly Analog | Reset de fase con posición sub-muestra, sin corrección BLEP | `vault/poly.rs:193-195` [V] |
| Sub del Acid 303 | Cuadrada ingenua (`if phase < 0.5 {1} else {-1}`) | `acid303.rs:456-458` [V] |

### 3.2 Comparación

| Tema | Nosotros | Referencias | Diferencia que se oye |
|---|---|---|---|
| Ancho de banda de las tablas | Se elige el nivel cuyo armónico superior queda **bajo Nyquist** (`table.rs:176-183`). Justo al pasar un umbral, el nivel siguiente tiene la mitad de armónicos, así que el contenido puede acabar cerca de fs/4 (≈12 kHz a 48 kHz) [V por cálculo sobre el código] | Surge: "strictly band-limited, yet still cover the entire audible spectrum" [V https://surge-synthesizer.github.io/] y 2× fijo en osciladores y filtros [V https://github.com/surge-synthesizer/surge/issues/3784]. Vital: oversampling 1×/2×/4×/8× [V https://davidmvogel.com/docs/Vital/UserGuide/Advanced] | Notas agudas algo más oscuras que en Surge o Vital, y un pequeño salto de brillo al cruzar cada octava [S: no medido] |
| Warps que crean discontinuidades | Sync y Quantize siguen aliasing (lo admite el informe 10); solo se mitiga con un nivel más fino | Brandt, "Hard Sync Without Aliasing" (ICMC 2001): insertar un **MinBLEP** en cada salto [V https://www.cs.cmu.edu/~eli/papers/icmc01-hardsync.pdf]; Vital y Surge sobremuestrean | Sync con barrido de mod: chirrido metálico de alias [S] |
| Modulación rápida de tono | Coeficientes y modulación cada 32 muestras (`wavetable/mod.rs:99`): el incremento de fase es escalonado | Plaits interpola frecuencia y PW **por muestra** con `ParameterInterpolator` [V https://github.com/pichenettes/eurorack/blob/master/plaits/dsp/oscillator/oscillator.h, MIT] | Vibrato rápido o FM por matriz con "zipper" [S] |
| Unison / supersaw | Separación lineal de voces, desafinación `100·det²` cents máx. ±100 (`mod.rs:746`), voces centrales a 1 y el resto a `blend`, fase inicial aleatoria opcional | Szabo (2010) mide el super saw del JP-8000 y propone curvas de desafinación y mezcla no lineales [V que el estudio existe: https://www.adamszabo.com/internet/adam_szabo_how_to_emulate_the_super_saw.pdf; detalle de las curvas: S, no lo transcribí] | El "supersaw" clásico se acerca más con curva no lineal; hoy los primeros 30 % del mando casi no desafinan (por el cuadrado) [V cálculo] |
| Pulso 303 | PolyBLEP con un offset lineal "hueco" (`acid303.rs:453`) | Open303 genera la cuadrada pasando la sierra por un `tanh` con drive, offset y desfase propios (`setTanhShaperDrive`, `set303SquarePhaseShift`) [V https://raw.githubusercontent.com/RobinSchmidt/Open303/main/Source/DSPCode/rosic_Open303.h] | Timbre de la cuadrada 303 |

![Página de Surge XT: lista de 12 algoritmos de oscilador, unison de 16 voces y la frase "strictly band-limited, yet still cover the entire audible spectrum"; abajo empieza el "Filter Block". https://surge-synthesizer.github.io/](img/02-surge-xt-osciladores-filtros.png)

![Issue #3784 de Surge: un colaborador explica que "Oscillators and filters are already 2x oversampled" y que el 2× es una constante del código. https://github.com/surge-synthesizer/surge/issues/3784](img/02-surge-issue-oversampling-2x.png)

![Guía de usuario de Vital (documentación de terceros): ajuste de oversampling 1× Draft, 2× Recommended, 4× y 8×. https://davidmvogel.com/docs/Vital/UserGuide/Advanced](img/02-vital-oversampling.png)

![Cabecera MIT de `plaits/dsp/oscillator/oscillator.h` (Mutable Instruments): oscilador con FM lineal y PolyBLEP de "this sample / next sample". El código STM32 de Mutable es MIT, así que se puede seguir de cerca con aviso en NOTICE. https://github.com/pichenettes/eurorack/blob/master/plaits/dsp/oscillator/oscillator.h](img/02-plaits-oscillator-polyblep.png)

### 3.3 Qué haría

* **Niveles a media octava o fundido entre niveles.** Con 20 niveles (en vez de 10) el peor caso sube de fs/4 a ~fs/2,8; con fundido lineal entre `level` y `level+1` según la fracción de octava desaparece el salto. Coste: memoria ×2 o una segunda lectura por muestra. Es el mismo truco que ya usa `granular/source.rs:1-12` ("reads the two levels around log2(s) and cross-fades").
* **2× opcional por voz para warps duros** (Sync, Quantize, PM): sintetizar a 2× y diezmar con el `Halfband` de `dsp/src/oversample.rs` (ya probado: 10 coeficientes, ~−100 dB, `:184-190`). Activarlo solo cuando el warp lo necesita, como hace Vital con su ajuste global [S sobre el criterio interno de Vital].
* **MinBLEP/PolyBLEP en el reset de sync** del Poly Analog (`poly.rs:193`): la posición sub-muestra ya se calcula, falta restar el residuo del salto (Brandt 2001). Coste bajo.
* **Interpolar el incremento por muestra** dentro del bloque de 32 (rampa lineal, como Plaits). Coste: una suma por voz y muestra.
* **Curva de unison**: sustituir `det²` por una curva medida (Szabo) y espaciar las voces de forma no uniforme; es un cambio de tabla, sin riesgo de licencia.
* **Unificar `dsp::Wavetable` y `MipTable`**: hay dos implementaciones de lo mismo con parámetros distintos (8× lineal frente a 4× Hermite). Mantener una.

---

## 4. Filtros virtual analog

### 4.1 Lo que hay, filtro por filtro

| Filtro | Topología real | Sobremuestreo | Comentario |
|---|---|---|---|
| SVF (wavetable, sampler, drum, shaper) | TPT de Simper, exacto bajo modulación | 1× | Bien. `dsp/src/svf.rs:110-140` [V] |
| Ladder del wavetable | 4 polos TPT; se resuelve `y4` del lazo **lineal** y después se aplica `tanh(x − k·y4)` solo a la entrada; `k = 4,2·r` | 1× | `filter.rs:150-154, 229-245` [V]. Con drive alto la `tanh` a 1× genera alias [S] |
| `Cascade4` (Vault: Mono Ladder y Poly OTA) | 4 polos TPT con saturación racional; el lazo global usa **la salida de la muestra anterior** (`self.last`) | "2×": se procesa la misma `x` dos veces y se promedian las salidas | `vault/kit.rs:265-309` [V]. El promedio de 2 es un diezmado de caja (−3 dB en fs/4, rechazo pobre) [V teoría] |
| `Diode18` (Acid 303) | 3 polos **independientes**, `s += g·(u − s)` con `g = tan(πfc/2fs)` (no es TPT), realimentación de la muestra anterior, `soft()` en la entrada y en el primer polo | "2×": dos pasos con la misma entrada, se queda con el segundo; sin filtro de diezmado | `acid303.rs:142-163` [V] |
| Comb ± | Retardo fraccionario con **interpolación lineal** | 1× | `filter.rs:250-256` [V]: amortigua agudos y desafina picos altos [V teoría] |
| Formant | 3 band-pass SVF con vocales A–U | 1× | `filter.rs:166-181` [V] |

### 4.2 Qué hacen las referencias

* **Zavalishin**, *The Art of VA Filter Design* rev. 2.1.2 (PDF gratuito, no es código) [V https://www.discodsp.net/VAFilterDesign_2.1.2.pdf]: método TPT/ZDF completo, escalera de transistores y de diodos [S: capítulos no releídos para este informe]. Es la base que ya citan 11 y 12, pero no se aplicó al lazo global ni a la escalera de diodos.
* **Huovilainen** (DAFx-04) y **Stinchcombe** (análisis de la escalera de transistores y derivadas) [V https://dafx.de/paper-archive/2004/P_061.PDF, https://www.timstinchcombe.co.uk/synth/Moog_ladder_tf.pdf]: no linealidad en cada etapa y sobremuestreo para afinar la resonancia.
* **Open303** (MIT): `TeeBeeFilter` es "a variation of the Moog ladder filter which includes a highpass in the feedback path that reduces the resonance on low cutoff frequencies… a highpass and an allpass filter in the input path"; tiene un modo `TB_303` con coeficientes propios, corte mínimo de 200 Hz y resonancia sesgada `(1−e^(−3r))/(1−e^(−3))` [V https://raw.githubusercontent.com/RobinSchmidt/Open303/main/Source/DSPCode/rosic_TeeBeeFilter.h]. Alrededor: highpass antes y después del filtro, envolvente de acento con ataque de 3 ms y decaimiento fijo de 200 ms en el 303 original, slide de 60 ms, amp decay de 3–4 s [V `rosic_Open303.h`, misma carpeta].
* **Surge XT** (GPL-3): además de sus LP/HP/BP/notch/allpass/comb, "K35 and Diode Ladder filter types from Odin 2… 12 and 24 dB/oct multimode filters from OB-Xd, and… Cutoff Warp, Resonance Warp and Tri-Pole filters by Jatin Chowdhury" [V https://surge-synthesizer.github.io/]; algunos tipos (vintage ladders) llevan un 2× adicional [V issue #3784].
* **Kahles, Esqueda y Välimäki**, "Oversampling for Nonlinear Waveshaping: Choosing the Right Filters", JAES 67(6), 2019: interpolación lineal ecualizada basta para subir; el diezmado es donde hay que invertir [V resumen https://aaltodoc.aalto.fi/items/3d3a2f3d-022a-4b48-98a5-a172c79dfb7a].
* **fundsp** (MIT/Apache) trae `moog` (lowpass de 4.º orden "Moog ladder") y resonadores, útiles como oráculo de respuesta en tests [V README de https://github.com/SamiPerttu/fundsp, líneas de la tabla de filtros].

![Cabecera de `rosic_TeeBeeFilter.h` de Open303 (MIT): el filtro 303 como variante de la escalera con highpass en la realimentación y highpass + allpass a la entrada. https://github.com/RobinSchmidt/Open303/blob/main/Source/DSPCode/rosic_TeeBeeFilter.h](img/02-open303-teebeefilter.png)

### 4.3 Qué haría

1. **Un módulo `aitrack_dsp::ladder`** con tres variantes y un solo núcleo: escalera de transistores (4 polos), OTA (saturación en entrada y realimentación, compensación de graves) y escalera de diodos (4 polos acoplados). ZDF con la no linealidad dentro del lazo, resuelta con la aproximación "lineal + una iteración" o con un paso de Newton (Zavalishin cap. de no linealidades [S]). Vault, wavetable y Acid lo comparten: menos código y el mismo carácter en todo el producto.
2. **Correrlo dentro de `dsp::oversample::Oversampler` a 2×** (interpolación y diezmado de media banda reales en lugar de repetir la entrada). La latencia del `Halfband` es de pocas muestras (`oversample.rs:139-145`) y en un instrumento no rompe el PDC.
3. **Acid 303 rehecho siguiendo Open303** (MIT): highpass en la realimentación, highpass y allpass de entrada, modo TB_303, cuadrada por `tanh` de la sierra, acento con ataque de 3 ms y decaimiento fijo. Al ser MIT podemos acercarnos mucho; si un bloque queda muy próximo, una sola línea en `NOTICE` (regla de `CLAUDE.md`).
4. **Comb con Hermite** (`dsp::delay::hermite`, `dsp/src/delay.rs:85`) o con un allpass de primer orden para la parte fraccionaria: afina los picos agudos.
5. **Más tipos baratos** que los demás tienen y aquí faltan: K35 (Sallen-Key no lineal) y SEM 12 dB (SVF con saturación). Fuente: Zavalishin y Pirkle; nada de copiar Odin/OB-Xd (GPL-3).

---

## 5. Envolventes, LFO y modulación

| Tema | Nosotros | Referencias |
|---|---|---|
| ADSR base | Ataque **lineal**, decay/release exponenciales con sobretiro de 0,001 para acabar en tiempo finito (`dsp/src/envelope.rs:352-466`) | Estándar; Open303 usa un "AnalogEnvelope" RC [V `rosic_Open303.h` incluye `rosic_AnalogEnvelope.h`] |
| Envolventes del wavetable | AHDSR con curvas −100..100 % (`CurveEnv`, informe 10) | Surge: DAHDSR en cada LFO y **MSEG de hasta 128 nodos**, más un modulador de fórmula en Lua [V https://surge-synthesizer.github.io/]; en 1.4 la fórmula pasa a 16 salidas [V nightly] |
| LFO dibujable | `LfoCurve` con curva por punto, one-shot, fase, fade (informe 10) | Vital: LFO como curva editable; Surge: MSEG con bucle |
| Matriz | 32 slots en espacio normalizado con "via" (`wavetable/modulation.rs:1-23`) | Equivalente a Vital/Vaporizer (informe 10) |
| Modulación en sampler, drum, kick, acid | **Ninguna**: el sampler tiene ADSR + filtro + envolvente de filtro (`sampler.rs:30-54`), el drum solo tune/decay/cutoff por pad | Decent Sampler tiene `<modulators>` (LFO y envolventes asignables) [V índice de https://decentsampler-developers-guide.readthedocs.io/en/latest/the-groups-element.html]; sfizz tiene LFO/EG flexibles (`FlexEnvelope`, `LFO` en su `src/sfizz`) [V captura del árbol de archivos] |

Lo que falta no es otra matriz para el wavetable sino **sacar la matriz a `stock/src/common`** para que sampler, drum, granular y kick tengan al menos 2 LFO y 1 envolvente asignables con la misma API de arrastrar y soltar. Y convertir `LfoCurve` en un MSEG de verdad (bucle entre dos puntos, modo envolvente con sustain), que cubre el 80 % de lo que hace el MSEG de Surge sin un editor nuevo.

![Changelog nightly de Surge XT: "We are approaching version 1.4.0 release" (13-oct-2026), editor de wavetables por script en Lua y Formula Modulator con 16 salidas. https://surge-synthesizer.github.io/nightlychangelog/](img/02-surge-xt-nightly-1-4.png)

---

## 6. Voces, glide, prioridad y MPE

| Tema | Nosotros | Referencias |
|---|---|---|
| Robo de voces | Wavetable: la más vieja ya liberada, si no la más vieja; fundido de 64 muestras (`wavetable/mod.rs:100, 1272-1277`). Vault: igual con ~1,3 ms (informe 11). Drum: la más vieja, 32 voces | Estándar |
| Glide | Exponencial de tiempo constante, constante de tiempo `glide_ms/4` por bloque de control (`mod.rs:669-679`); en poly sale de la última nota tocada (`mod.rs:1267`) | Decent Sampler distingue `glideMode` (p. ej. legato), `returnToHeldNote` y `glideRetrigger` [V captura]; falta en nuestro sampler y en el kick |
| Prioridad mono | Última nota con pila de 16 (wavetable, Vault, Acid) | Igual en las referencias |
| Retrigger del Acid | Al empezar una frase, el VCA salta a 1,0 de golpe (`acid303.rs:293, 333`) | Open303 usa envolventes RC con ataque [V cabecera]. Posible clic si la nota anterior aún sonaba [S] |
| `Note.slide` del modelo | Sin consumidor en el motor | Ver §1 |
| MPE | `Expression` trae Tuning, Pressure, Pan, Brightness y Volume (`dsp/src/event.rs:26-37`). Solo el wavetable lee **Tuning y Pressure** (`mod.rs:1346-1356`); Brightness (CC74/timbre) se ignora; Vault, sampler, acid, kick y granular no leen ninguna. El arpegiador descarta las expresiones (`midi/arp.rs:28-30`) | MPE v1.1 (2022) sustituye a la 1.0 sin cambios técnicos [V https://midi.org/mpe-midi-polyphonic-expression]; Surge tiene modo MPE y su Formula expone `mpe_enabled` [V nightly] |

Cambio mínimo con impacto: que **Brightness, Pan y Volume** entren como fuentes por voz en la matriz del wavetable (3 entradas nuevas en `SOURCES`, que es de solo añadir) y que Vault Poly Analog y el sampler lean Tuning y Pressure. Así un controlador MPE suena expresivo en los tres instrumentos con más uso polifónico.

![Documentación de Decent Sampler 1.43.1: instrumentos monofónicos con `silencedByTags`, `returnToHeldNote`, `glideMode` y `glideTime`. https://decentsampler-developers-guide.readthedocs.io/en/latest/the-groups-element.html](img/02-decentsampler-grupos-silencedbytags.png)

![Página de la MIDI Association: la especificación M1-100-UM MPE v1.1 sustituye a la rp53 v1.0 "but has no technical design changes". https://midi.org/mpe-midi-polyphonic-expression](img/02-mpe-v1-1.png)

---

## 7. Sampler, SFZ, streaming y drum

### 7.1 Interpolación

Leemos con Hermite cúbico de 4 puntos (`sample.rs:78-84`, `dsp/src/delay.rs:85`). Está bien a tono original o hacia abajo, pero hacia arriba no hay filtro previo: con +12 st el contenido entre fs/4 y fs/2 del sample se pliega [V teoría; no medido]. En platos y hats transpuestos se oye como "brillo sucio" [S].

| Opción | Calidad | Coste por muestra y canal | Referencia |
|---|---|---|---|
| Hermite (hoy) | Imágenes ≈ dependientes del sobremuestreo de la fuente; sin antialias al subir | 4 lecturas | Niemitalo, "Polynomial Interpolators for High-Quality Resampling of Oversampled Audio" [V https://yehar.com/blog/wp-content/uploads/2009/08/deip.pdf] |
| Mips de media banda + Hermite, fundido entre dos niveles | Antialias proporcional al tono, misma lectura | 8 lecturas + memoria ×2 | **Ya implementado** en `granular/source.rs:1-27` (31 taps, 4 niveles) [V] |
| Sinc enventanada con corte escalado (kernel ensanchado al subir) | La mejor | 8–72 taps | sfizz ofrece Nearest, Linear, Hermite3, B-spline3 y Sinc8…Sinc72 [V captura]; el opcode SFZ `sample_quality` va de 1 (lineal) a 10 (sinc con ventanas mayores) [V https://sfzformat.com/opcodes/sample_quality/]; Smith, *Digital Audio Resampling* [V https://ccrma.stanford.edu/~jos/resample/] |

**Recomendación:** reutilizar el `MipBuf` del granular para sampler y drum (calidad "normal") y dejar una sinc polifásica de 16–32 taps para render offline o un ajuste "Alta calidad". Cumple PLAN §5 sin pagar la sinc en tiempo real por cada voz.

![`Interpolators.h` de sfizz (BSD-2): modelos Nearest, Linear, Hermite3, B-spline3 y sinc enventanada de 8 a 72 puntos. https://github.com/sfztools/sfizz/blob/develop/src/sfizz/Interpolators.h](img/02-sfizz-interpoladores-sinc.png)

### 7.2 Streaming de disco

Nuestro diseño (`sample.rs:17-24, 193-437`): zonas de más de 10 s se abren con 16 384 frames de precarga y 6 páginas de 16 384 frames en anillo; el hilo de audio pide la página que falta escribiendo `want_page` y un hilo la carga. Problemas, todos leídos en el código:

* **Una sola petición por zona** (`want_page` es un `AtomicU32`, `:361, 372`): dos voces de la misma zona en sitios distintos se pisan la petición.
* **Desalojo con polifonía**: el comentario de seguridad supone que el worker "does not overwrite the oldest slot for STREAM_SLOTS pages" (`:364-365`); con varias voces leyendo páginas distintas esa suposición no se cumple y una página puede reescribirse mientras se lee [S: no reproducido, pero la condición lo permite].
* **Se reabre el WAV en cada página** (`WavReader::open`, `:319`) y el worker sondea cada 2 ms (`:432`): latencia variable de disco.
* **Un fallo de página devuelve 0** (`:369-372`): un hueco de silencio, sin contador ni aviso.
* Se busca la página recorriendo los 6 slots en **cada uno de los 4 taps** de Hermite y por canal (`:341-375`).

sfizz resuelve lo mismo con otra idea: cada archivo tiene una precarga (8 192 frames por defecto, `preloadSize` en `Config.h`) y, cuando una voz lo usa, un hilo carga **el resto del archivo entero en memoria**, publicando `availableFrames` a medida que avanza; un contador de lectores decide cuándo liberarlo (estados `Preloaded`, `Streaming`, `Done`, `GarbageCollecting`) [V https://raw.githubusercontent.com/sfztools/sfizz/develop/src/sfizz/FilePool.h, BSD-2]. Es más memoria en pico, pero no hay desalojos, la lectura es un slice contiguo y el único caso de fallo (leer más allá de `availableFrames`) es detectable.

**Recomendación:** pasar a ese modelo (por archivo, no por zona, compartido con la caché `DECODED` de `sample.rs:95-105`), con el archivo abierto una vez, carga por bloques grandes y despertar del worker por canal (`rtrb` ya está en el workspace) en vez de sondear. Para bibliotecas enormes, añadir más tarde un anillo **por voz** en vez de por zona.

### 7.3 Cobertura SFZ, round robin y choke

Opcodes que entiende `sfz.rs` [V grep de cadenas]: `sample, lokey, hikey, key, pitch_keycenter, lovel, hivel, seq_length, seq_position, trigger (attack/release/first/legato), loop_mode, loop_start, loop_end, offset, end, transpose, tune, volume, pan, default_path, release_key` y las cabeceras `<control> <global> <group> <region>`.

| Falta | Para qué sirve | Referencia |
|---|---|---|
| `group` + `off_by`, `polyphony` | **Choke** (hi-hat abierto/cerrado), límites de voces | https://sfzformat.com/opcodes/ y https://sfzformat.com/opcodes/polyphony/ [V] |
| `lorand`/`hirand` | Round robin aleatorio | https://sfzformat.com/opcodes/lorand/ [V] |
| `amp_veltrack`, `amp_velcurve_N`, `ampeg_*`, `fil_*` | Que el instrumento suene como lo diseñó su autor | sfzformat.com [S: no abrí cada página] |
| `xfin_*`/`xfout_*` | Fundidos entre capas de velocidad | ídem [S] |
| `sw_*` (keyswitches), `locc/hicc` | Articulaciones | ídem [S] |
| `<master>`, `#define`, `#include` | Muchas bibliotecas reales no cargan sin ellos | ídem [S] |
| Crossfade de bucle en SFZ | Hoy las zonas SFZ van con `xfade: 0.0` (`sampler.rs:608`) | `loop_crossfade` [S] |

Round robin: un contador por tecla (`sampler.rs:435-445`); sirve para el caso básico. El **drum** no tiene ni capas de velocidad ni round robin por pad (un sample por pad, `drum.rs:202-222`) y su choke es interno a los 16 pads con fundido de 5 ms (`drum.rs:9-10, 281-292`); el choke entre cadenas del Drum Rack sigue pendiente (20, trabajo 2.9). Decent Sampler resuelve el choke con etiquetas (`silencedByTags`) [V captura] y Geonkick con modos de reproducción "full, note off, cut, choke groups" [V README].

---

## 8. Granular

Es el instrumento más cuidado: mips antialias con fundido, ventanas que valen cero en los extremos, normalización por solapamiento y determinismo por posición de transporte (`granular/mod.rs:16-50`). Cita a Mutable Clouds (MIT) como referencia de comportamiento, que es la referencia correcta. No veo carencias algorítmicas comparables a las de §4 y §7; las mejoras serían de modulación (matriz común, §5) y de lectura en sampler/drum (llevar sus mips allí, §7.1).

---

## 9. Percusión sintética: Kick 808 frente a Geonkick

Nuestro bombo: seno de fase continua, barrido exponencial de tono, "beater" de dos senos con decaimiento de 2,8 ms, saturación asimétrica y paso bajo de un polo (`kick808.rs:226-292`). La saturación va a 1× (`:276-280`); con un seno puro el alias es bajo, pero el drive alto sobre el click sí pliega [S].

Geonkick (GPL-3+) [V README y captura]: 16 instrumentos por kit, **3 capas con 3 osciladores cada una** (seno, cuadrada, triángulo, sierra, ruido blanco y browniano, sample), envolventes de amplitud, frecuencia, densidad de ruido y pitch del sample, filtro LP/BP/HP con envolvente de corte y resonancia, distorsión con envolvente de drive, FM OSC1→OSC2, limitador, exportación a WAV/FLAC/SFZ y modos "full, note off, cut, choke groups".

La distancia no es de calidad sino de **forma**: nosotros tenemos curvas fijas con un mando por tramo; Geonkick dibuja cada envolvente. Propuesta: que el tono, la amplitud y el click sean `LfoCurve` en modo envolvente (ya existe el tipo y su editor en el wavetable), añadir una capa de ruido filtrado (para snares y claps, convirtiéndolo en un "Drum Synth" pequeño como el de Live 13 que cita 13 §3) y pasar el drive por `dsp::shaper::Adaa1` (`dsp/src/shaper.rs:76`).

![README de Geonkick en GitLab: 3 osciladores por capa, envolventes por parámetro, FM, distorsión y "Playmode: full, note off, cut, choke groups"; licencia GPL-3.0 o posterior. https://gitlab.com/quamplex/geonkick](img/02-geonkick-features.png)

---

## 10. Efectos MIDI y FM

### 10.1 Efectos MIDI

Lo que está bien y conviene proteger: **determinismo** (arp por índice absoluto de paso, `arp.rs:3-19`; velocity/random/echo por hash de semilla y posición, `velocity.rs:11-16`, `random.rs:14-18`, `echo.rs:12-16`), contabilidad de note-off con `Ledger` y tests por posición de transporte. Ningún sinte open source de los estudiados tiene una capa de efectos MIDI así (Surge, Vital, Odin traen arpegiador o secuenciador internos, no dispositivos encadenables) [S: por catálogo de sus páginas].

Huecos concretos [V en las cabeceras]:

* `midi.chord`: "Not in this device: strum, chance per voice and learn" (`chord.rs:21`). Strum es lo que más se usa en house y lo-fi.
* `midi.arp`: 14 estilos, swing, offset, repeticiones y 8 pasos de velocidad (`arp.rs:102-120`), pero sin patrón de pasos con silencio/ligadura/transposición por paso ni ratchets, y descarta las expresiones MPE (`arp.rs:28-30`).
* Nadie emite ni consume `Note.slide` (§1): un `midi.*` que convierta `slide` en solape (legato) haría que el Acid y los sintes en modo legato lo respeten sin tocar el motor.

### 10.2 FM6 frente a Dexed/MSFA

FM6 es propio (12 algoritmos, tabla de 2048 puntos, realimentación promediada; informe 11). MSFA, el núcleo de Dexed, es **Apache-2.0** [V COPYING], así que su modelo de envolventes de 4 tasas/4 niveles, el escalado por tecla y la tabla de niveles del DX7 se pueden estudiar y seguir de cerca con atribución en `NOTICE` (Apache exige conservar el aviso). Dexed en sí es GPL-3 [S] y solo vale para entender. Esto importa si algún día se quieren importar bancos SysEx de DX7 (Dexed v1.0.1 sigue activo, [V captura]).

![Release v1.0.1 de Dexed (29-nov-2025): arregla el escalado de 0.9.9 y alinea la versión del plugin con la del proyecto. https://github.com/asb2m10/dexed/releases/tag/v1.0.1](img/02-dexed-v1-0-1.png)

### 10.3 Ecosistema Rust

* **fundsp** (MIT/Apache) [V README]: `poly_saw` (PolyBLEP), osciladores de tabla band-limited (`saw`, `square`, `triangle`, `pulse`, `soft_saw`, `organ`, `hammond`), `dsf_saw`/`dsf_square` (fórmula de suma discreta), filtros `moog` y resonadores, y un componente `oversample`. Avisa de que su oscilador de tabla "allocates memory in the form of global wavetables". Úsese como **oráculo en tests** (comparar espectros), no como dependencia del hilo RT.
* **nih-plug** (ISC) sigue en mantenimiento [V]: nada nuevo respecto a PLAN §5.
* **Monique** es MIT a elección (no es Rust, pero es el único mono-synth completo con licencia permisiva de la lista) [V captura].

![Licencia de Monique: "licensed under either the GPL3… or the MIT license". https://github.com/surge-synthesizer/monique-monosynth/blob/main/LICENSE](img/02-monique-licencia-dual.png)

![Changelog de Odin 2: 2.4.0 (rediseño completo de la UI) y 2.4.1. https://github.com/TheWaveWarden/odin2/blob/master/change_log.md](img/02-odin2-changelog.png)

---

## 11. Mejoras priorizadas

Esfuerzo: S = días, M = 1–2 semanas, L = más. Riesgo de licencia: "ninguno" = papers o código propio; "bajo" = MIT/BSD/Apache seguido de cerca (anotar en `NOTICE` si un bloque queda muy próximo).

| # | Qué cambiar | Archivos | Algoritmo / referencia | Esfuerzo | Impacto | Riesgo de licencia |
|---|---|---|---|---|---|---|
| 1 | **Módulo único de filtros VA** (transistor, OTA, diodos) con no linealidad dentro del lazo ZDF, corriendo en `Oversampler` 2× con media banda real; sustituye `ladder_tick`, `Cascade4` y `Diode18` | nuevo `dsp/src/ladder.rs`; `stock/src/instrument/wavetable/filter.rs`, `vault/kit.rs`, `acid303.rs` | Zavalishin 2.1.2; Huovilainen DAFx-04; Stinchcombe; Kahles et al. 2019 | M | Alto: resonancia afinada en agudos, menos alias con drive, carácter coherente | Ninguno |
| 2 | **Acid 303 al estilo Open303**: highpass en realimentación, HP+allpass de entrada, modo TB_303, cuadrada por `tanh` de la sierra, acento con ataque 3 ms / decaimiento 200 ms, VCA con ataque en vez de salto a 1,0, sub con PolyBLEP | `acid303.rs:128-163, 293, 333, 453-458` | Open303 `TeeBeeFilter` y `Open303` | S–M | Alto en el instrumento con más identidad del producto | Bajo (MIT) |
| 3 | **Antialias al transponer en sampler y drum** reutilizando el `MipBuf` del granular (fundido entre dos niveles) | `instrument/sample.rs`, `drum.rs:366-381`, `sampler.rs`; mover `granular/source.rs` a un sitio común | Mips de media banda (ya nuestro) | S–M | Alto en hats/platos/vocales transpuestos hacia arriba | Ninguno |
| 4 | **Streaming por archivo al estilo sfizz**: precarga + carga completa en segundo plano con `availableFrames` y contador de lectores; archivo abierto una vez; worker despertado por cola; contador de fallos visible | `instrument/sample.rs:193-437`, `sampler.rs:282-340` | sfizz `FilePool` | M | Alto en fiabilidad (sin huecos de silencio con polifonía) | Bajo (BSD-2, solo la idea) |
| 5 | **Sinc polifásica** (16–32 taps, corte escalado) como calidad "Alta" y en render offline | `instrument/sample.rs`, `dsp/src` | Smith, Digital Audio Resampling; sfizz Sinc8…72 | M | Medio: cierra PLAN §5 | Ninguno |
| 6 | **SFZ: `group/off_by`, `polyphony`, `lorand/hirand`, `amp_veltrack`, `ampeg_*`, `#define`/`#include`, `<master>`, crossfade de bucle** | `instrument/sfz.rs`, `sampler.rs:426-466, 608` | sfzformat.com | M | Alto en compatibilidad con bibliotecas reales | Ninguno |
| 7 | **Niveles de wavetable a media octava o fundido entre niveles** y unificar `dsp::Wavetable` con `MipTable` | `wavetable/table.rs:60-70, 176-219`, `dsp/src/wavetable.rs` | Mip-mapping clásico | S | Medio: agudos más brillantes y sin saltos | Ninguno |
| 8 | **2× opcional por voz para warps duros** y BLEP en el sync del Poly Analog | `wavetable/mod.rs:818-880`, `vault/poly.rs:193` | Brandt 2001 (MinBLEP); `Halfband` propio | M | Medio: sync y quantize sin chirrido | Ninguno |
| 9 | **MPE completo**: Brightness/Pan/Volume como fuentes por voz; Vault Poly y sampler leen Tuning/Pressure; el arp conserva expresiones si la nota sale de una sola tecla | `wavetable/mod.rs:1346-1356`, `wavetable/modulation.rs` (`SOURCES`), `vault/poly.rs`, `sampler.rs` | MPE v1.1 | S–M | Medio (alto para quien tiene controlador MPE) | Ninguno |
| 10 | **Matriz de modulación común** (2 LFO + 1 envolvente mínimo) para sampler, drum, granular y kick; `LfoCurve` con bucle y modo envolvente (MSEG ligero) | nuevo `stock/src/common/modmatrix.rs`; `wavetable/modulation.rs` | Surge MSEG, Vital (solo como idea) | M–L | Medio-alto en uso | Ninguno (idea; no código GPL) |
| 11 | **Kick 808 → drum synth**: envolventes dibujables para tono/amp/click, capa de ruido filtrado, drive con ADAA | `kick808.rs` | Geonkick (solo idea), `dsp::shaper::Adaa1` | M | Medio: snares/claps propios | Ninguno |
| 12 | **Drum: capas de velocidad y round robin por pad** | `drum.rs` | Decent Sampler, SFZ `seq_*` | M | Medio: baterías menos "ametralladora" | Ninguno |
| 13 | **Unison con curva medida** y espaciado no uniforme; interpolación del incremento de fase por muestra | `wavetable/mod.rs:741-768` | Szabo 2010; Plaits `ParameterInterpolator` | S | Medio en supersaws y vibratos | Ninguno / bajo (MIT) |
| 14 | **Comb con Hermite o allpass fraccionario**; ADAA en la distorsión del FX interno | `wavetable/filter.rs:250-256`, `synth_fx.rs:147-157` | `dsp::delay::hermite`, `Adaa1` | S | Bajo-medio | Ninguno |
| 15 | **Strum en `midi.chord`** y patrón por pasos en `midi.arp`; un `midi.*` que traduzca `Note.slide` a legato | `midi/chord.rs`, `midi/arp.rs` | Live Chord/Arpeggiator (función, no código) | S–M | Medio | Ninguno |

Orden sugerido: 1 → 2 (comparten el módulo de filtros), 3 → 4 → 6 (sampler usable con bibliotecas reales), luego 7, 9 y 13 (rápidos), y por último 5, 8, 10, 11, 12.

---

## 12. Qué no verifiqué

* No medí espectros ni CPU: las afirmaciones de "se oye" llevan [S]. Los tests de aliasing existentes (informe 10 §5) cubren el wavetable sin warps duros; no cubren sampler, Acid ni Vault.
* No leí el código de Vital ni de Surge para confirmar cómo hacen cada cosa internamente: solo documentación, páginas y el issue citado.
* El riesgo de carrera en el streaming (§7.2) es una inferencia del código, no un fallo reproducido.
* No abrí cada página de opcode SFZ marcada [S] en §7.3.

## 13. Fuentes

* Surge XT: https://surge-synthesizer.github.io/ · https://surge-synthesizer.github.io/nightlychangelog/ · https://surge-synthesizer.github.io/changelog/ · https://github.com/surge-synthesizer/surge/issues/3784 · https://github.com/surge-synthesizer/surge/milestones
* Vital: https://github.com/mtytel/vital · https://vital.audio/ · https://davidmvogel.com/docs/Vital/UserGuide/Advanced (terceros)
* Odin 2: https://github.com/TheWaveWarden/odin2/blob/master/change_log.md · https://librearts.org/2025/05/odin-2-4-0/
* Dexed / MSFA: https://github.com/asb2m10/dexed/releases/tag/v1.0.1 · https://github.com/google/music-synthesizer-for-android/blob/master/COPYING
* OB-Xd: https://www.discodsp.com/obxd/ · https://github.com/NickCulbertson/OB-Xd-GPL
* Monique: https://github.com/surge-synthesizer/monique-monosynth/blob/main/LICENSE
* Mutable Instruments (MIT): https://github.com/pichenettes/eurorack · https://github.com/pichenettes/eurorack/blob/master/plaits/dsp/oscillator/oscillator.h
* VCV/Cardinal: https://vcvrack.com/manual/PluginLicensing · https://github.com/DISTRHO/cardinal
* sfizz (BSD-2): https://github.com/sfztools/sfizz · https://github.com/sfztools/sfizz/blob/develop/src/sfizz/Interpolators.h · https://raw.githubusercontent.com/sfztools/sfizz/develop/src/sfizz/FilePool.h · https://raw.githubusercontent.com/sfztools/sfizz/develop/src/sfizz/Config.h
* SFZ: https://sfzformat.com/opcodes/ · https://sfzformat.com/opcodes/sample_quality/ · https://sfzformat.com/opcodes/lorand/ · https://sfzformat.com/opcodes/polyphony/
* Decent Sampler: https://decentsampler-developers-guide.readthedocs.io/en/latest/the-groups-element.html · https://www.decentsamples.com/product/decent-sampler-plugin/
* Geonkick: https://gitlab.com/quamplex/geonkick · https://quamplex.com/2025/07/03/geonkick-version-3-6-0-has-been-released/
* Open303 (MIT): https://github.com/RobinSchmidt/Open303 · https://raw.githubusercontent.com/RobinSchmidt/Open303/main/Source/DSPCode/rosic_TeeBeeFilter.h · https://raw.githubusercontent.com/RobinSchmidt/Open303/main/Source/DSPCode/rosic_Open303.h · https://raw.githubusercontent.com/RobinSchmidt/Open303/main/License.txt
* Ardour: https://manual.ardour.org/plugins-instruments/
* Rust: https://github.com/SamiPerttu/fundsp · https://github.com/robbert-vdh/nih-plug
* MPE: https://midi.org/mpe-midi-polyphonic-expression · https://midi.org/the-midi-association-announces-the-v1-1-update-to-the-midi-polyphonic-expression-mpe-specification
* Papers: Brandt, *Hard Sync Without Aliasing* (ICMC 2001) https://www.cs.cmu.edu/~eli/papers/icmc01-hardsync.pdf · Szabo, *How to Emulate the Super Saw* (KTH 2010) https://www.adamszabo.com/internet/adam_szabo_how_to_emulate_the_super_saw.pdf · Niemitalo, *Polynomial Interpolators…* https://yehar.com/blog/wp-content/uploads/2009/08/deip.pdf · Smith, *Digital Audio Resampling* https://ccrma.stanford.edu/~jos/resample/ · Zavalishin, *The Art of VA Filter Design* 2.1.2 https://www.discodsp.net/VAFilterDesign_2.1.2.pdf · Huovilainen DAFx-04 https://dafx.de/paper-archive/2004/P_061.PDF · Stinchcombe https://www.timstinchcombe.co.uk/synth/Moog_ladder_tf.pdf · Kahles, Esqueda, Välimäki, JAES 2019 https://aaltodoc.aalto.fi/items/3d3a2f3d-022a-4b48-98a5-a172c79dfb7a
