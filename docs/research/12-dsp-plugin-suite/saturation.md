# Familia de saturación: Saturator, Tape, Amp, Console

Cuatro dispositivos en `crates/stock/src/fx/saturator` (todo el código es nuestro; ninguna línea copiada ni traducida de proyectos GPL). Editores compactos en `crates/app/src/views/device_ui/saturator*`; la ventana de dispositivo usa el mismo render con `dev.big`.

| Id | Nombre | Qué es | Categoría del picker |
|---|---|---|---|
| `fx.saturator` | Saturator | Multibanda (1–3 bandas LR4), 9 estilos con ADAA1, sobremuestreo 1×/2×/4×, bloqueo de DC, tono, mezcla | Audio Effects → Saturation & Distortion |
| `fx.tape` | Tape | Histéresis Jiles–Atherton (RK2 con subpasos), bias, saturación, head bump, pérdida HF, velocidad, wow & flutter, anchura | Audio Effects → Saturation & Distortion |
| `fx.amp` | Amp | Pedales y previo por circuitos: Centaur (Klon), Screamer (TS‑808), Fuzz, Amp (dos triodos + tone stack Bassman), cabina | Audio Effects → Saturation & Distortion |
| `fx.console` | Console | Par Channel/Buss (sin/asin, idea Airwindows), modo Glue (compresor RMS suave + etapa de consola), "Iron" (saturación de transformador en graves) | Audio Effects → Saturation & Distortion (también encaja en Multiband/Mastering como "glue") |

## 1. Saturator (mejora de `fx.saturator`)

### Antialiasing por antiderivada (ADAA1)
Para una no linealidad sin memoria `f`, en lugar de `y[n] = f(x[n])` se emite la media de `f` sobre el segmento entre las dos últimas entradas:

`y[n] = (F(x[n]) − F(x[n−1])) / (x[n] − x[n−1])`, con `F' = f`.

Equivale a filtrar con una caja continua (sinc) la señal ya deformada antes de muestrear, con lo que los armónicos que se pliegan quedan atenuados (Parker, Zavalishin & Le Bivic 2016; Bilbao, Esqueda, Parker & Välimäki 2017). Si `|x[n] − x[n−1]| < 1e-6` se usa `f` en el punto medio. Retardo: media muestra. Todo en `f64` (la diferencia de antiderivadas cancela mal en `f32` con drive alto). Nota: ADAA1 en la zona lineal es el promedio de dos muestras (−3 dB a fs/4), por eso se combina con el sobremuestreo IIR polifásico existente (2×/4×), no se usa sola a 1× para material brillante.

### Curvas (todas con pendiente unidad en el origen tras normalizar por `f'(0)`)
| Estilo | `f(x)` | `F(x)` |
|---|---|---|
| Tape | `tanh x` dentro de pre/de‑énfasis (+6 dB por encima de 3 kHz, inversa exacta) | `ln cosh x` |
| Diode | `x≥0: 1−e^{−x}`; `x<0: −k(1−e^{x/k})` (`k = 1 − 0.6·bias`) | `x + e^{−x} − 1`; `−kx + k²(e^{x/k} − 1)` |
| Tube | `tanh(x+b) − tanh b` (`b = 0.6·bias`) | `ln cosh(x+b) − x tanh b − ln cosh b` |
| Transistor | `g(x+b) − g(b)`, `g(u) = u/√(1+u²)` (`b = 0.8·bias`) | `√(1+(x+b)²) − x g(b) − √(1+b²)` |
| Clip | `clamp(x, ±1)` | `x²/2` / `|x| − 1/2` |
| Rectify | clip con la mitad negativa ×`k`, `k = 1 − 2·bias` (−1 = onda completa, octava arriba) | `k·C(x)` en `x<0` |
| Fold | `sin x` | `1 − cos x` |
| Tri Fold | triángulo de periodo 4 | periódica a trozos (`u²/2 − u`, `3u − u²/2 − 4`, `u = (x+1) mod 4`) |
| Bitcrush | cuantización + retención de muestra | (sin ADAA: el aliasing es el efecto) |

Compatibilidad: el rango del parámetro Style pasó de 0..4 a 0..8 y los cinco estilos originales están en los índices pares, así que el valor normalizado guardado `k/4` sigue cayendo en el mismo estilo (`2k/8`). Test `old_style_values_keep_their_style`; los 12 presets antiguos no cambiaron ni un byte al regenerarlos.

Bloqueo de DC: un polo HP a 6 Hz tras el diezmado (los estilos asimétricos generan DC). Test `asymmetric_styles_leave_no_dc` (< 1e‑3).

Mediciones (tono de 4 987 Hz a −6 dBFS, drive +18 dB, energía no armónica relativa a los armónicos, ventana Blackman‑Harris):

| Estilo | 1× | 2× | 4× |
|---|---|---|---|
| Tape | −20.8 | −48.5 | −64 |
| Diode | −25.4 | −56.1 | −64 |
| Tube | −23.7 | −62.1 | −64 |
| Transistor | −24.1 | −59.2 | −64 |
| Clip | −20.9 | −49.4 | −63 |
| Rectify | −20.7 | −48.6 | −64 |
| Fold | −14.6 | −62.4 | −62 |
| Tri Fold | −1.3 | −29.2 | −51 |

(−64 dB es el suelo de la medición.)

## 2. Tape (`fx.tape`)

### Modelo de histéresis (Jiles–Atherton, resuelto como Chowdhury 2019)
Magnetización anhisterética `M_an = M_s·L(Q)`, `Q = (H + αM)/a`, Langevin `L(x) = coth x − 1/x`, `L'(x) = 1/x² − 1/sinh² x` (series de Taylor para `|x| < 1e‑3`; un solo `exp` por evaluación).

`dM/dH = [ (1−c)·δ_M·(M_an − M) / ((1−c)·δ·k − α(M_an − M)) + c·(M_s/a)·L'(Q) ] / [1 − c·α·(M_s/a)·L'(Q)]`

con `δ = sign(dH)` y `δ_M = 1` si `δ` y `M_an − M` tienen el mismo signo. Integración RK2 (punto medio) sobre el camino de `H` (`dM = dH·dM/dH`, así la frecuencia de muestreo se cancela). **Subpasos adaptativos**: un paso de campo mayor que 0.25 se divide en hasta 8 subpasos lineales; sin ello, RK2 a 2× con material caliente y brillante se vuelve ruido (aliasing medido −2.5 dB → −38.6 dB a 2× y −75 dB a 4× con un tono de 5 kHz a +12 dB). Guardas: `c` limitado a [0.05, 0.95] para que el denominador irreversible no pase por cero, reinicio si `M` no es finita o supera `4·M_s`.

Mapeo de controles (diseño propio inspirado en ChowTape):
- `M_s = 0.5 + 1.5·(1 − saturation)`
- `a = M_s / (0.01 + 6·drive)`
- `c = 0.05 + 0.9·√bias` (cinta infrapolarizada = lazo ancho, más distorsión de histéresis; la AC bias real linealiza)
- `α = 1.6e‑3`, `k = 0.47875`
- Campo `H = 4·x` (0 dBFS satura bien; −20 dBFS queda casi lineal). La ganancia en pequeña señal del lazo se mide en cada cambio con una simulación de 192 pasos y se compensa (`makeup`), así Drive cambia el carácter más que el nivel.

### Reproducción
- **Head bump**: campana `+6·bump dB` a `f_b = 3.4·ips` Hz (≈ 51 Hz a 15 ips, 102 Hz a 30 ips) y un valle de `−0.4·` esa ganancia a `2.3·f_b`.
- **Pérdida por separación cabeza–cinta** (Wallace): `|H| = e^{−2π f d / v}`. El −3 dB está en `f = ln√2 · v / (2π d)`; `d` = 0.2…3.2 µm según HF Loss, `v = ips·0.0254`. Se aproxima con un paso bajo SVF críticamente amortiguado (Q = 0.5) en ese punto. (La pérdida de entrehierro `sinc(π f g / v)` queda fuera de banda para entrehierros de pocos µm.)
- **Wow & flutter**: línea de retardo modulada (Hermite cúbica), wow ≈ 0.6 Hz + deriva aleatoria suavizada (máx. 2.5 ms), flutter a 12 y 19.7 Hz (máx. 0.08 ms). El retardo oscila entre 0 y 2·profundidad, así que con 0 % no hay retardo ni latencia; no se reporta como latencia (como en una máquina real).
- Anchura M/S, salida, mezcla (seco alineado con la latencia del sobremuestreo).

## 3. Amp (`fx.amp`)

### WDF
Ondas de tensión `a = v + R i`, `b = v − R i` (Fettweis 1986). Elementos lineales por transformada bilineal: condensador `R = T/2C`, `b[n] = a[n−1]`; fuente de tensión resistiva `b = V_s`. Adaptador paralelo de 3 puertos adaptado (`b₃ = Σ γᵢ aᵢ`, `γᵢ = Gᵢ/G₃`, `bᵢ = a₃ + b₃ − aᵢ`). La no linealidad (par de diodos) va en la raíz, resuelta en forma cerrada con la omega de Wright (Werner et al. 2015):

`b = a + 2R I_s − 2 nV_t · ω( ln(R I_s / nV_t) + (a + R I_s)/nV_t )` (diodo que conduce; signo de `a` para el par).

`ω(x)` (solución de `w + ln w = x`): estimación a trozos + 4 pasos de Newton (residuo < 1e‑9; test). Test `diode_reflection_satisfies_shockley`: la onda reflejada cumple Kirchhoff + Shockley con error relativo < 1e‑6 para Si (1N4148: `I_s = 2.52 nA`, `n = 1.752`), Ge (1N34A: `I_s = 200 nA`, `n = 1.3`) y LED.

Circuito: fuente resistiva ∥ C ∥ par de diodos = el recortador RC clásico (pasa bajos `1/2πRC` en pequeña señal, recorte en el codo).

### Modos
- **Centaur** (Klon): etapa de ganancia (+6…+42 dB) con corte de graves (HP 400 Hz) → recortador WDF de germanio (R 1 k, fc 4.8 kHz); suma limpio + recortado con mezcla ligada a la ganancia (`0.3 + 0.65·gain`), como el potenciómetro doble del original; control de agudos activo = shelf ±12 dB a 1.6 kHz.
- **Screamer** (TS‑808): ganancia no inversora `1 + (51 k + drive)/4.7 k` aplicada solo por encima de ~720 Hz (el "mid hump"); los diodos de realimentación se modelan como recortador WDF de silicio cuyo polo sigue al condensador de 51 pF sobre `51 k + drive`; se suma la señal limpia (etapa no inversora); tono = LP fijo de 723 Hz + agudos variables. Test: a la misma ganancia un tono de 1 kHz distorsiona > 10 dB más que uno de 60 Hz.
- **Fuzz**: HP de acoplamiento 90 Hz, +20…+56 dB, offset de bias (transistor "hambriento": asimetría y gating) → recortador WDF asimétrico (Si en un sentido, Ge en el otro) → bloqueo de DC → tono pasivo LP 400 Hz / HP 1.2 kHz con mezcla (scoop de medios).
- **Amp**: dos etapas de triodo (`tanh` sesgada con ADAA1, condensador de acoplamiento entre ellas) → **tone stack Fender '59 Bassman** en forma cerrada (Yeh & Smith 2006), 3.er orden, `R1 = 250 k, R2 = 1 M, R3 = 25 k, R4 = 56 k, C1 = 250 pF, C2 = C3 = 20 nF`, discretizado por bilineal en `f64` (DF2T). Verificado contra un análisis nodal independiente del circuito FMV (coinciden al 0.1 dB en 80 Hz/500 Hz/5 kHz para 7 combinaciones de potenciómetros; el control de medios real es suave, ~5.5 dB a 500 Hz). Potenciómetro de graves con curva logarítmica (`l = bass²`).
- **Cabinet**: Off / 1x12 abierta / 2x12 combo / 4x12 cerrada: HP resonante (70–90 Hz), resonancia baja (+3…+5 dB a 100–120 Hz), presencia (1.8–2.8 kHz) y dos pasos bajo del altavoz (4.2–7 kHz). Filtros, no IR: cero memoria, cero latencia, coste fijo.

Todo el circuito corre a 2× o 4×; los tonos y la cabina a la frecuencia del host. Aliasing a 4× con un tono de 3 kHz y 70 % de ganancia < −30 dB en los cuatro modos (test). La WDF no lleva ADAA: el siguiente paso sería ADAA en WDF (Albertini, Bernardini & Sarti 2020).

## 4. Console (`fx.console`)

- **Channel** codifica con `sin(x)` (limitado a ±π/2); **Buss** decodifica la suma con `asin(x)` (limitado a ±1). Un canal solo vuelve idéntico (`asin(sin x) = x`; test sample a sample a 1×), pero la suma de canales codificados no: `asin(Σ sin xᵢ)` deja productos cruzados dependientes del nivel (intermodulación impar `2f₁ − f₂`, `2f₂ − f₁`; test entre −70 y −20 dB), que es el "sonido de suma" de una consola. La idea es de Airwindows Console (Chris Johnson, MIT; PurestConsole usa el par sin/asin); la implementación es propia y corre a 2× por defecto. Drive entra y sale de la curva (`f(g x)/g`), así Channel y Buss al mismo Drive siguen siendo inversas exactas.
- **Glue**: detector RMS enlazado (30 ms), rodilla suave de 12 dB, umbral `−10 − 14·glue` dBFS, ratio `1 + 1.5·glue` (hasta 2.5:1), ataque 10 ms / release 180 ms, y luego una etapa "amplificador de consola" (`tanh` con sesgo 0.15 → algo de 2.º armónico). Medidor de reducción de ganancia.
- **Iron** (todos los modos): por debajo de 120 Hz la señal pasa por `tanh` con drive `1 + 5·iron`, mezclada por `√iron` — el núcleo de un transformador de salida satura antes en graves. Test: a 50 Hz el 3.er armónico sube > 10 dB y a 2 kHz casi no cambia.
- Respuesta plana con señal baja en los tres modos (±0.3 dB a 100 Hz / 1 kHz / 15 kHz).

## 5. CPU (instancia estéreo, 48 kHz, bloques de 128)
Tests `cpu_budget` en cada módulo (presupuesto en release; en debug se permite 60×) y el bench del registro (`cargo test -p aitrack-stock --release --test bench -- --ignored`); ver la tabla del informe final.

## 6. Proyectos de referencia y licencias
| Proyecto | Licencia | Qué se tomó |
|---|---|---|
| ChowTapeModel (Jatin Chowdhury) | BSD‑3 | Formulación JA y esquema RK2 del artículo DAFx‑19, mapeo drive/sat/width como inspiración. Código reescrito con diseño propio (subpasos adaptativos, calibración de ganancia, filtros de reproducción analíticos); no hay bloques portados → no hace falta entrada en NOTICE (si el integrador lo prefiere, una línea "ChowTapeModel, BSD‑3, inspiración del modelo de histéresis"). |
| ChowCentaur / BYOD (Chowdhury) | BSD‑3 / GPL‑3 | Solo la idea de modelar el Klon por etapas con WDF (BYOD es GPL: solo ideas). |
| chowdsp_wdf | BSD‑3 | Referencia de la fórmula del par de diodos con ω de Wright; nuestra versión se deriva de Shockley + Kirchhoff y se verifica en tests. |
| Airwindows (Console, PurestConsole, BussColors) | MIT | Idea del par sin/asin entre canal y bus. Implementación propia, sin código portado; NOTICE opcional: "Airwindows Console concept (MIT)". |
| Surge XT, Vital, vibez, Lemon‑Studio, Pro‑Wave | GPL‑3 | Nada. |

## Bibliografía
- J. Parker, V. Zavalishin, E. Le Bivic, "Reducing the aliasing of nonlinear waveshaping using continuous-time convolution", DAFx‑16, Brno, 2016.
- S. Bilbao, F. Esqueda, J. Parker, V. Välimäki, "Antiderivative antialiasing for memoryless nonlinearities", IEEE Signal Processing Letters 24(7), 2017.
- J. Chowdhury, "Real-time physical modelling for analog tape machines", DAFx‑19, Birmingham, 2019.
- D. C. Jiles, D. L. Atherton, "Theory of ferromagnetic hysteresis", J. Magnetism and Magnetic Materials 61, 1986.
- R. L. Wallace, "The reproduction of magnetically recorded signals", Bell System Technical Journal 30, 1951 (pérdida por separación).
- A. Fettweis, "Wave digital filters: theory and practice", Proc. IEEE 74(2), 1986.
- K. J. Werner, V. Nangia, A. Bernardini, J. O. Smith III, A. Sarti, "An improved and generalized diode clipper model for wave digital filters", AES 139th Convention, 2015.
- K. J. Werner, "Virtual analog modeling of audio circuitry using wave digital filters", tesis doctoral, CCRMA Stanford, 2016.
- S. D'Angelo, L. Gabrielli, L. Turchet, "Fast approximation of the Lambert W function for virtual analog modelling", DAFx‑19, 2019.
- D. T. Yeh, J. O. Smith III, "Discretization of the '59 Fender Bassman tone stack", DAFx‑06, Montreal, 2006.
- D. T. Yeh, J. S. Abel, J. O. Smith III, "Simulation of the diode limiter in guitar distortion circuits by numerical solution of ordinary differential equations", DAFx‑07, 2007.
- D. Albertini, A. Bernardini, A. Sarti, "Antiderivative-based antialiasing for nonlinear wave digital filters", DAFx‑20, 2020 (trabajo futuro).
- U. Zölzer (ed.), *DAFX: Digital Audio Effects*, 2.ª ed., Wiley 2011, cap. 4 (no linealidades) y 12 (análogo virtual).
- W. Pirkle, *Designing Audio Effect Plugins in C++*, 2.ª ed., Focal Press 2019, cap. 19 (no linealidades, sobremuestreo).
- J. D. Reiss, A. McPherson, *Audio Effects: Theory, Implementation and Application*, CRC 2014, cap. 7 (distorsión).
- V. Zavalishin, *The Art of VA Filter Design*, rev. 2.1.2, 2020 (filtros TPT/SVF usados en tono, cabina y reproducción).
- J. O. Smith III, *Physical Audio Signal Processing*, CCRMA, 2010 (WDF y modelos de circuito).
