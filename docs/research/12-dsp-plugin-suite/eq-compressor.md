# EQ dinámico y compresor (`fx.eq`, `fx.compressor`)

Tarea D2 de la suite de plugins propios. Los dos dispositivos conservan sus ids
(`fx.eq`, `fx.compressor`) y todos los ids de parámetro anteriores; lo nuevo
entra con ids más altos, así que los proyectos y presets viejos cargan igual.

## 1. EQ (`crates/stock/src/fx/eq/`)

### 1.1 Estructura

| Archivo | Qué hace |
|---|---|
| `mod.rs` | Parámetros, bandas IIR, dinámica, enrutado L/R/M/S, curva, meter, `latency()`. |
| `analog.rs` | Prototipos analógicos complejos `H(jΩ)` de cada forma (referencia de los modos Natural/Linear). |
| `fir.rs` | Convolución particionada uniforme (UPOLS) estéreo con matriz 2×2 y crossfade. |
| `tests.rs` | Respuesta en frecuencia, simetría, fase, M/S, dinámica, sidechain, NaN/denormales, presupuesto de CPU. |

8 bandas: campana, shelf grave/agudo, corte grave/agudo (Butterworth de 6 a
96 dB/oct), notch, pasa-banda y tilt de primer orden. Cada banda trabaja en
estéreo, mid, side, left o right.

### 1.2 Modos de fase (`Phase`, id 99)

**Minimum** (latencia 0). Es el motor de antes:

- *Matched*: las campanas, shelves y pasa-bandas estáticos son biquads
  ajustados en magnitud según Vicanek: polos por *matched-z* (invarianza al
  impulso de los polos analógicos), y el numerador se resuelve para que `|H|²`
  coincida con el prototipo analógico en DC, en Nyquist y en `ω0`. Así no hay
  *cramping* cerca de Nyquist.
- *Classic*: SVF TPT (Zavalishin) en todas las bandas.
- Las bandas dinámicas, los cortes, el notch y el tilt van siempre en SVF TPT,
  que aguantan bien la modulación por muestra.

**Natural** (latencia `b + D_nat` = 256 + 256 = 512 muestras a 48 kHz). Se
muestrea la respuesta compleja **del prototipo analógico** (magnitud *y* fase)
en una rejilla de `n = 4096` puntos hasta Nyquist, se le añade un retardo
global `D_nat` y se convierte en FIR por muestreo en frecuencia (IFFT y
ventana). El resultado tiene la fase de un EQ analógico y nada de cramping.
El pequeño pre-anillo que nace de la discontinuidad en Nyquist cae en los
primeros `D_nat` taps: una ventana de semicoseno lo suaviza al entrar, y otra
suaviza el final de la cola.

**Linear** (latencia `b + n/2` = 2304 muestras a 48 kHz, 4096 taps). Igual,
pero con la **magnitud** analógica y fase cero, centrada en `n/2`, y una
ventana Tukey (α = 0,25). La respuesta al impulso es simétrica (está
testeado): no hay distorsión de fase. La resolución es de unos `fs/n` ≈ 12 Hz,
así que una campana muy estrecha por debajo de ~40 Hz pierde precisión. Es el
compromiso habitual de un EQ de fase lineal de latencia media.

La geometría escala con la frecuencia de muestreo para mantener el mismo
intervalo de tiempo (≈ 85 ms): 4096 taps hasta 50 kHz, 8192 hasta 100 kHz y
16384 por encima (`fir_geometry`). `latency()` informa de la latencia del
modo actual, y el motor recalcula la compensación de retardo cuando cambia.

#### Matriz 2×2 exacta

Cada banda es una matriz 2×2 compleja por frecuencia en la base L/R:

- estéreo: `diag(h, h)`;  left: `diag(h, 1)`;  right: `diag(1, h)`;
- mid: `[[a, b], [b, a]]` con `a = (h+1)/2`, `b = (h−1)/2`;
- side: `a = (1+h)/2`, `b = (1−h)/2`.

El EQ completo es el producto en orden `B7·…·B0` (las bases L/R y M/S no
conmutan, por eso se multiplica de verdad en lugar de sumar curvas). El FIR
lleva los cuatro elementos `LL, LR, RL, RR`. Los términos cruzados solo se
calculan si alguna banda está en mid o side.

#### Convolución (UPOLS)

Overlap-save con particiones de `b = 256` taps (Wefers, *Partitioned
convolution algorithms for real-time auralization*, 2015; Gardner, JAES 1995):

- por bloque y canal, una FFT real de `2b` puntos de la ventana de entrada,
  guardada en una línea de retardo en frecuencia;
- `Y = Σ_k X[n−k]·H_k`;
- una IFFT por canal de salida, y la segunda mitad es la salida válida.

El coste por muestra no depende de `n`, salvo en el MAC complejo
(`n/b` particiones). Un rediseño se escribe en el segundo juego de filtros y
entra con un crossfade lineal a lo largo de un bloque: mover un nodo no hace
clic (testeado con un barrido de ganancia por bloque). Los rediseños se
limitan a uno cada 2048 muestras mientras algo se mueve. Todo se reserva en
`prepare`: `design` y `tick` usan las llamadas `*_with_scratch` de realfft,
sin asignaciones.

#### Dinámica en los modos FIR

La parte estática de cada banda (forma, ganancia, enrutado) va en el FIR. El
cambio dinámico de una banda dinámica se aplica después, como una SVF de
fase mínima con ganancia `Δ`, detectada sobre la señal retardada (el
sidechain se retarda la misma latencia). Así la dinámica sigue siendo exacta
a nivel de muestra sin rediseñar el FIR cada pocos ms. Esa parte dinámica,
pequeña, es de fase mínima; Pro-Q y ZL Equalizer hacen el mismo
compromiso en sus modos de baja latencia.

### 1.3 Dinámica por banda

Detector: un pasa-banda SVF centrado en la banda (Q de la banda en campanas y
pasa-bandas, 0,7 en shelves), seguidor de pico con ataque y release propios.
El nivel en dB pasa por el computador de ganancia de Giannoulis et al. con
rodilla cuadrática (`Dyn Knee`), y el resultado se limita a `Dyn Range`. Hay
cuatro direcciones (`Dyn Mode`):

| Modo | Fórmula | Uso |
|---|---|---|
| Cut Above | `Δ = −min(gc(L), R)` | de-esser, domar resonancias |
| Boost Above | `Δ = +min(gc(L), R)` | expansión hacia arriba, transitorios |
| Cut Below | `Δ = −min(gc(2T−L), R)` | expansión hacia abajo, puerta de banda |
| Boost Below | `Δ = +min(gc(2T−L), R)` | compresión hacia arriba, rellenar |

`gc(x) = gain_computer(x, T, ratio, knee)`. Las direcciones *below* reflejan
el nivel respecto al umbral. Con los valores por defecto (Cut Above,
rango 24, rodilla 6) se obtiene exactamente el comportamiento anterior.

**Sidechain externo por banda.** El EQ declara `has_sidechain: true`. Con
`Sidechain = External` (id 100, el interruptor que pone la ficha de
sidechain), las bandas con `Dyn Source = External` detectan sobre el bus de
sidechain que entrega el motor (`ProcessContext::sidechain`, con el mismo
enrutado M/S/L/R). Ejemplos: el bombo hunde el grave del bajo (preset
*Kick-Bass Carve*) o la voz abre un hueco en los medios de la música (*Vocal
Pocket*).

### 1.4 Analizador y colisiones

- Espectro post-EQ, de 128 puntos (STFT con ventana de Hann, como antes).
- Espectro del sidechain, de 48 puntos, cuando hay sidechain (meter
  `[267..315)`, con la marca `[266]`).

La vista dibuja la **colisión** donde los dos espectros superan el suelo y
quedan cerca: `fuerza = clamp((min(a,b) + 60)/36) · clamp(1 − |a−b|/18)`.
Se pinta como una franja en rojo bajo la curva, para saber dónde tallar.
Todo el layout del meter (315 valores) cabe en `DEVICE_METER_LEN = 320`.

### 1.5 Parámetros nuevos

| Id | Nombre | Rango |
|---|---|---|
| 99 | Phase | Minimum / Natural / Linear |
| 100 | Sidechain | Internal / External |
| `104 + 8b + 0` | Band N Dyn Mode | Cut Above / Boost Above / Cut Below / Boost Below |
| `104 + 8b + 1` | Band N Dyn Range | 0–24 dB (por defecto 24) |
| `104 + 8b + 2` | Band N Dyn Source | Internal / External |
| `104 + 8b + 3` | Band N Dyn Knee | 0–24 dB (por defecto 6) |

## 2. Compresor (`crates/stock/src/fx/compressor.rs`)

Computador de ganancia en el dominio logarítmico con rodilla cuadrática, y
suavizado del detector *smooth decoupled* (con variante *branching*), según
Giannoulis, Massberg y Reiss (JAES 2012).

### 2.1 Estilos

Son modelos de comportamiento, no emulaciones de un aparato concreto:

| Estilo | Detector / tiempos | Rodilla |
|---|---|---|
| Clean | decoupled, tiempos tal cual | la elegida |
| Punch | branching, ataque ×1,6, release ×0,6 | la elegida |
| Glue | RMS ≥ 60 %, release dependiente del programa | la elegida |
| Opto | ataque ×2, release que se alarga con la memoria de reducción (dos etapas) | ≥ 6 dB |
| **VCA** | pico puro, ataque branching, **release lineal en dB** (20 dB por tiempo de release, como los VCA clásicos especificados en dB/s) | la elegida |
| **Vari-Mu** | ataque ×1,5, RMS ≥ 50 %, release dependiente del programa | ≥ 18 dB (la relación crece con el nivel) |

`Style` (id 6, Clean/Punch/Glue/Opto) sigue igual. El nuevo `Model` (id 18:
Classic / VCA / Vari-Mu) elige los modelos nuevos; en Classic se usa `Style`.
Son dos ids para que ningún preset antiguo cambie de significado (los
valores se guardan normalizados).

### 2.2 Topología (id 19)

- **Feed-forward**: el detector escucha la entrada o el sidechain.
- **Feedback**: el detector escucha la salida del propio compresor (la
  muestra anterior). Si el computador tiene pendiente `R − 1` sobre el
  umbral, `g = (R−1)(y−T)` con `y = x − g` da `y − T = (x − T)/R`: la misma
  curva estática que feed-forward (está testeado). Al cerrar el lazo, el
  ataque efectivo se acelera en torno a `R` mientras la salida está sobre el
  umbral; por debajo el lazo se abre y suelta al ritmo del release. De ahí
  el carácter más suave de los compresores feedback.
- **Estabilidad**: linealizando, el lazo discreto es `g[n] = g[n−1](1 − αR) + …`,
  así que hace falta `αR < 2`. Se limita `α ≤ 1/R` (`feedback_min_ms`), que
  da un lazo críticamente amortiguado a cualquier ajuste (testeado con
  relación 100 y ataque de 0,01 ms). Con sidechain externo siempre se usa
  feed-forward.

### 2.3 Sidechain

Se usa el bus interno o el externo. El detector pasa por un HPF de 12 dB/oct
(apagado a 20 Hz), una campana `SC EQ` (frecuencia y ganancia, Q 1, apagada
a 0 dB; ids 20–21) y un LPF de 12 dB/oct (id 22, apagado a 20 kHz). Con esto
salen el de-esser de banda ancha, la compresión de bajo que no bombea con el
sub, etc. `SC Listen` reproduce la señal del detector.

### 2.4 Resto

- Lookahead de hasta 10 ms, con la latencia informada.
- Mezcla en paralelo, rango, makeup manual o automático.
- Meter: la GR se muestra con retención de pico y caída de 40 dB/s, para que
  el historial de la vista (60 puntos/s) no se pierda ningún golpe.
- Layout del meter (70 valores): GR, pico de entrada y de salida, la curva
  estática en 64 pasos, la latencia, el estilo en efecto y si la topología
  feedback está activa.

## 3. Vistas

- `device_ui/eq.rs`:
  - la curva dibuja el prototipo analógico en los modos FIR (es lo que
    suena);
  - las ganancias dinámicas son con signo (la barra sube o baja);
  - menú de banda con dirección, fuente y rango;
  - chip de fase y chip de sidechain;
  - franja de colisión y espectro del sidechain;
  - en grande (ventana del dispositivo) muestra además la fila de dinámica
    completa de la banda seleccionada.
- `device_ui/dynamics.rs`: un único selector de estilo de 6 opciones (escribe
  `Style` y `Model` a la vez), la topología, el SC EQ, y en grande una
  columna extra con los controles de sidechain.

## 4. Pruebas y CPU

Pruebas, todas en `#[cfg(test)]` junto al código:

- magnitud contra la analógica (Matched, Linear);
- simetría de la IR lineal y latencia exacta;
- fase Natural contra la analógica (< 2°);
- M/S exacto en los modos FIR;
- crossfade sin clics;
- las cuatro direcciones dinámicas;
- sidechain externo en Minimum y Linear;
- curva estática en feedback;
- release lineal del VCA;
- estabilidad del feedback;
- SC EQ/LPF;
- NaN y denormales;
- presupuesto de CPU por instancia (solo en release).

Las asignaciones dentro de `process` y `meter` las cubre
`crates/stock/tests/alloc.rs`, que recorre todo el registro con cada
parámetro en 0, en 1 y en valores pseudoaleatorios (incluye los modos FIR).

## 5. Bibliografía

- J. O. Smith, *Introduction to Digital Filters with Audio Applications*
  (CCRMA, 2007), cap. de transformada bilineal y *matched-z*; *Spectral Audio
  Signal Processing* (CCRMA, 2011), cap. «FIR digital filter design»
  (muestreo en frecuencia, ventanas) y «Overlap-add/Overlap-save».
- M. Vicanek, «Matched Second Order Digital Filters», nota técnica, 2016.
- R. Bristow-Johnson, «Cookbook formulae for audio EQ biquad filter
  coefficients» (Audio EQ Cookbook) — prototipos analógicos de campana y
  shelves.
- U. Zölzer (ed.), *DAFX: Digital Audio Effects*, 2.ª ed., Wiley 2011, cap. 2
  (filtros, EQ paramétrico, shelving) y cap. 4 (procesado dinámico).
- V. Zavalishin, *The Art of VA Filter Design*, rev. 2.1.2, 2020 — SVF TPT.
- W. G. Gardner, «Efficient convolution without input-output delay», JAES
  43(3), 1995.
- F. Wefers, *Partitioned Convolution Algorithms for Real-Time
  Auralization*, Logos Verlag 2015 (UPOLS).
- D. Giannoulis, M. Massberg, J. D. Reiss, «Digital Dynamic Range
  Compressor Design — A Tutorial and Analysis», JAES 60(6), 2012.
- J. D. Reiss, A. McPherson, *Audio Effects: Theory, Implementation and
  Application*, CRC 2014, cap. 6 (compresores feed-forward y feedback).
- W. C. Pirkle, *Designing Audio Effect Plugins in C++*, 2.ª ed., Focal 2019,
  caps. de EQ paramétrico y dinámica.

## 6. Proyectos OSS consultados (solo ideas, sin código copiado)

- **ZL Equalizer 2 / ZL Compressor** (instalados como plugins de referencia;
  GPL-3): ideas de flujo de trabajo, como la dinámica por banda con fuente
  de sidechain y el análisis de colisión. No se miró ni se copió código.
- **FabFilter Pro-Q / Pro-C** (comerciales): la nomenclatura de los modos
  Natural/Linear y los estilos como referencia funcional, solo desde su
  documentación pública.
- Ningún código de terceros se ha portado, así que no hay entrada nueva
  en `NOTICE`.
