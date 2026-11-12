# Investigación 11: un instrumento "biblioteca de clásicos" (Vault)

Fecha: 2026-11-12
Alcance: diseño e implementación del instrumento de serie `instrument.vault` (`crates/stock/src/instrument/vault/`): un solo dispositivo que aloja varios motores de síntesis clásicos, elegidos por preset, con cuatro macros por motor y una biblioteca de fábrica filtrable por tipo y carácter. La UI (navegador de presets y panel de macros) es una tarea posterior; aquí está la API sobre la que se construye.

**Licencias.** Todo el código es propio. Se estudiaron ideas publicadas en artículos (citados abajo). Proyectos GPL como Surge XT, Vital, Dexed, Helm, Odin 2 o ZynAddSubFX solo sirven como referencia conceptual; no se copió ni tradujo nada. Los nombres de presets y de motores son descriptivos ("ladder", "tine", "drawbar") y no usan marcas de terceros.

---

## 0. Resumen

| Pregunta | Decisión |
|---|---|
| ¿Un plugin por motor o uno solo? | Uno solo, `instrument.vault`, con el parámetro `Engine`. Así un preset cambia de motor y el navegador es uno. |
| ¿Cómo se cambia de motor sin asignar en el hilo RT? | Todos los motores se crean en `Vault::new()` / `prepare()`. Cambiar de motor es un cambio de parámetro: se libera el motor viejo, se hace un fundido de 20 ms y después se resetea. Sin asignaciones, sin locks. Lo verifica `tests/alloc.rs::vault_switches_engines_and_presets_without_rt_heap_traffic`. |
| ¿Formato de preset? | El formato de siempre (`presets/<plugin>/<Categoría>/<Nombre>.json`, mapa id → valor normalizado). La carpeta es el **tipo** de sonido y los `tags` son el motor más el **carácter**. Sale en el panel Presets de Cmd+K sin tocar la app. |
| ¿Macros? | Cuatro perillas (`Macro 1..4`) con su profundidad (`Macro n Depth`, −100..100 %). El mapeo es **por motor**: cada macro mueve varios parámetros con rango y curva. La macro 4 es "Space" (reverb y delay) en todos los motores. |
| ¿Nivel? | El generador renderiza cada preset y ajusta `Volume` para que la ventana de 400 ms más fuerte quede en −17 dBFS RMS con pico ≤ −1 dBFS. Un test lo comprueba. |

Motores:

| # | Motor | Arquitectura clásica en que se inspira |
|---|---|---|
| 0 | Mono Ladder | Monofónico de 3 osciladores con filtro en escalera de transistores de 24 dB |
| 1 | Poly Analog | Polifónico de 2 osciladores por voz con "poly-mod", filtro OTA de 24 dB y chorus BBD |
| 2 | FM6 | FM (en realidad modulación de fase) de 6 operadores con algoritmos |
| 3 | Electric Piano | Piano eléctrico de lengüeta metálica ("tine") o de lámina ("reed") con pastilla |
| 4 | Drawbar Organ | Órgano de ruedas fónicas con 9 drawbars, percusión, click y altavoz rotativo |
| 5 | String Machine | "String ensemble" divide-down con chorus de tres fases |
| 6 | Wavetable | El sintetizador wavetable de serie (`instrument.wavetable`), alojado |

---

## 1. Cómo funcionan los navegadores de presets con macros

Las bibliotecas tipo "Analog Lab" juntan muchos instrumentos detrás de una sola interfaz con tres capas:

1. **Navegador:** filtros por *tipo* (bajo, lead, pad, keys, pluck, metales, cuerdas, órgano, FX), por *carácter/estilo* (cálido, brillante, oscuro, agresivo, vintage, evolutivo…) y por *instrumento* (el motor). Búsqueda por texto y favoritos. Los favoritos son del usuario, no del preset.
2. **Cuatro macros** grandes, con nombre, que resumen el sonido ("Brillo", "Tiempo", "Movimiento", "Espacio"). Cada macro mueve varios parámetros a la vez con rangos elegidos por el diseñador del preset, de modo que cualquier posición suena bien.
3. **Vista avanzada** opcional con el panel completo del motor.

En Vault:

* **Tipo** = carpeta del preset (`vault::TYPES`).
* **Carácter** = `tags` del preset, de una lista cerrada (`vault::CHARACTERS`); el tag del motor (`ENGINE_TAGS`) también va en `tags`.
* **Motor** = parámetro `Engine` del preset.
* `vault::preset_info()` devuelve todo eso (y los nombres de las macros del motor) para que la UI filtre sin cargar ningún procesador.

Decisión: el mapeo de macros es **por motor**, no por preset. Así las macros tienen siempre el mismo significado dentro de un motor ("Cutoff" en el ladder, "Drawbars" en el órgano) y el preset solo elige dónde empiezan las perillas y con qué profundidad. Esto mantiene el preset como un mapa plano de parámetros (todo pasa por la Command API, deshacer incluido) y la UI puede leer los nombres con `macros::macros(engine)`. Si más adelante hace falta mapeo por preset, se pueden añadir slots de macro como parámetros nuevos (ids libres en el bloque global) sin romper presets existentes.

Modelo matemático de una macro (en `macros.rs`):

```text
efectivo = clamp(base + depth × lerp(from, to, curva(macro)), 0, 1)     (valores normalizados)
```

Con todas las macros a 0 se oye exactamente el patch base. Las curvas son Linear, Exp (x²), Log (√x), S (smoothstep) y Step (un interruptor, p. ej. rotor lento/rápido). La perilla del parámetro sigue mostrando su valor base; el motor lee el efectivo (`Vault::effective(id)`), así que la UI puede dibujar un "anillo" con el valor modulado.

---

## 2. Motores: arquitecturas clásicas y cómo se implementaron

### 2.1 Mono Ladder

* 3 osciladores PolyBLEP (sierra, cuadrada, triángulo, pulso del 25 %), cada uno con pie (32'..2'), desafinación en semitonos y nivel; más ruido.
* **Filtro en escalera:** cuatro polos de un polo TPT en cascada con realimentación global, con saturación (aproximación racional de tanh) en la entrada y en cada etapa, y a 2× de sobremuestreo para que la resonancia afine y auto-oscile. Huovilainen (DAFx-04) mostró que el modelo no lineal del circuito se reduce a una cascada de secciones de primer orden con no linealidades dentro, y que conviene sobremuestrear para afinar; Zavalishin (*The Art of VA Filter Design*) da la forma TPT. Rasgos del modelo: el grave baja al subir la resonancia (como en el circuito) y la auto-oscilación aparece cerca del 100 %. Test: `kit::tests::ladder_self_oscillates_at_cutoff_and_stays_bounded` (oscila a ±6 % del corte y queda acotado).
* Envolventes de filtro y amplitud (ADSR de `aitrack-dsp`), glide exponencial, legato con prioridad a la última nota (pila de 16 teclas), vibrato y LFO al corte, velocidad a corte y amplitud.

### 2.2 Poly Analog

* 16 voces, 2 osciladores por voz: A (sierra/pulso/triángulo) y B (sierra/pulso/triángulo/seno, ±24 st, fino), mezcla, ruido, **hard sync** de A a B.
* **Poly-mod** (la idea del polifónico clásico de 5 voces): dos fuentes por voz, la envolvente de filtro y el oscilador B, con destinos que se encienden por separado: frecuencia de A (FM exponencial), ancho de pulso de A y corte del filtro. Permite campanas metálicas, barridos de sync y sonidos "caóticos" con solo dos osciladores.
* **Filtro OTA de 24 dB:** la misma cascada de 4 polos, pero con saturación solo en la entrada y la realimentación y con compensación de ganancia en la banda de paso, que es lo que distingue sonoramente a los filtros de 4 polos con OTA de la escalera: conservan el grave con resonancia alta. Test: `ota_keeps_passband_gain`.
* **Deriva analógica:** dos fuentes aleatorias lentas por voz (tono, unos cents; corte, una fracción de octava), escaladas por `Drift`.
* **Chorus estéreo** de dos tomas (modos I, II, I+II, a la manera de los chorus BBD de los polifónicos de los 80) y reparto de voces en el estéreo (`Spread`).

### 2.3 FM6

* 6 operadores seno por voz (tabla de 2048 puntos con interpolación lineal, error ~1e-6), cada uno con ratio (½, 1..16 más fino), desafinación ±10 cents, nivel 0..99 (≈0,75 dB por paso), ADSR, sensibilidad a la velocidad y escalado por tecla.
* **12 algoritmos** propios (`fm::ALGORITHMS`): pila de 6, dos pilas, tres pares, pila de 4 + par, dos moduladores + pila, uno a tres, cinco a uno, pila de 3 + tres portadoras, par + cuatro, todos portadores (registro de órgano), pila de 5 + seno, dos pares + dos. Se representan como máscaras de bits; un modulador siempre tiene un índice mayor que su destino, así que una voz se evalúa en una sola pasada de op 6 a op 1 (lo comprueba un test).
* Realimentación en un operador por algoritmo, usando la media de las dos últimas salidas: es el truco conocido para que una realimentación alta no degenere en ruido.
* Globales: `Brightness` (escala a todos los moduladores, ideal para macro), `Env Time` (escala todos los tiempos), envolvente de tono (golpe inicial), vibrato y trémolo.
* Base teórica: Chowning (1973). El crate no tenía código FM que reutilizar (el wavetable solo tiene "phase-FM" como warp de oscilador), así que es una implementación nueva.

### 2.4 Electric Piano (modal)

Un piano de lengüeta es un voladizo (*cantilever*) golpeado por un martillo cuyo movimiento modula el flujo de una pastilla electromagnética. Falaize (DAFx-15) lo modela como martillo no lineal + viga amortiguada + transductor no lineal; Pfeifle (DAFx-17) hace modelos en tiempo real del tine y del reed con su pastilla. Nuestra versión, mucho más barata:

* **Tres modos** por voz con las razones de la viga empotrada-libre, 1 : 6,27 : 17,55 (fundamental, "campana" y el parcial del contacto del martillo), cada uno un resonador complejo amortiguado (rotación por muestra: afinación exacta, coste mínimo).
* **Golpe dependiente de la velocidad:** la amplitud crece con la velocidad al cuadrado y la dureza del martillo sube los modos altos, así que tocar fuerte es más brillante (test `electric_piano_velocity_changes_the_timbre`).
* **Pastilla polinómica:** centrada da armónicos impares; descentrada (`Pickup`), pares. La no linealidad crece con la velocidad: el "ladrido" (`Bark`) de las notas fuertes.
* **Reed:** decae más rápido, menos campana, pastilla más fuerte con recorte suave (el carácter más "sucio" del piano de lámina).
* Golpe de martillo con ruido filtrado (`Thump`), apagadores en la liberación (salvo en las teclas agudas, que no los tienen), desafinación estable por tecla, tono pasa-bajos, bloqueador de DC y **trémolo** de amplitud o **auto-pan** estéreo.

### 2.5 Drawbar Organ

* 9 senos por tecla a las razones de los pies 16', 5⅓', 8', 4', 2⅔', 2', 1⅗', 1⅓', 1' (0,5 : 1,5 : 1 : 2 : 3 : 4 : 5 : 6 : 8), con el doblado de octava en lo más agudo como las ruedas fónicas. Cada drawbar 0..8 a ≈3 dB por paso.
* **Percusión** de disparo único (solo si no hay otra tecla pulsada) en el 2.º o 3.er armónico, rápida o lenta, suave o normal; mientras está activa, el drawbar de 1' se apaga, como en el circuito original.
* **Key click:** ráfaga corta de ruido filtrado al pulsar y soltar.
* **Vibrato/chorus tipo scanner** (V1–V3, C1–C3) con una línea de retardo modulada a ~7 Hz; saturación de válvula; tono.
* **Altavoz rotativo:** cruce a 800 Hz entre bocina (agudos) y tambor (graves); cada rotor es una línea de retardo modulada (Doppler) más modulación de amplitud, captada por dos micrófonos a 90° para el estéreo. Lento/rápido con la inercia de cada rotor: la bocina, ligera, cambia en menos de un segundo; el tambor, pesado, tarda varios (`Rotor Ramp` la escala). Base: Smith, Serafin, Abel y Berners, "Doppler Simulation and the Leslie" (DAFx-02), que simula el Doppler con líneas de retardo interpoladas y dos oyentes fijos. Test: `organ_rotor_ramps_with_inertia`.

### 2.6 String Machine

* En los "string ensembles" de los 70 un reloj maestro se divide para todas las notas (*divide-down*), así que todas las teclas están en fase entre sí. Aquí cada oscilador saca su fase de un contador de muestras compartido (`fase = t·f mod 1`, en f64): mismo comportamiento de instrumento único y registros 16'/8'/4' siempre alineados.
* Registros: violonchelo 16', violín 8', viola 4' (sierras) y trompa 8' (cuadrada). Envolvente de ataque/liberación por tecla, un solo filtro pasa-bajos para todo (parafónico) con *swell* (se abre a medida que suben las envolventes) y pasa-altos.
* **Ensemble:** tres tomas moduladas a 120° con una componente lenta (~0,6 Hz) y otra rápida (~6 Hz). Es lo que convierte un órgano de sierras fino en una sección de cuerdas.

### 2.7 Wavetable (alojado)

El `WavetableSynth` de serie vive dentro de Vault; sus parámetros están en `WT_BASE + id` (`params::wt(id)`). Vault le pasa notas, bend y CC; sus cambios de parámetros llegan con un bloque de retraso como mucho (cambiar el motor y las macros son operaciones de bloque). Macros: Brillo (corte), Movement (la macro 1 propia del wavetable, que el preset enruta en su matriz), Carácter (posición de tabla + drive) y Space. Su estado propio (tablas de usuario, curvas de LFO) se guarda dentro del estado de Vault.

### 2.8 Común

Tras el motor: drive, estantes graves/agudos, delay ping-pong sincronizado al tempo, reverb FDN y volumen (la cadena de efectos compacta del wavetable, `synth_fx.rs`). Pedal de sostenido gestionado por Vault (difiere los note-off). Pitch bend con rango configurable. Salida saneada (NaN → 0) y sin denormales (`kit::flush` en estados y salida).

---

## 3. Tiempo real

* `Vault::new()` crea los 6 motores nativos, el wavetable, los búferes y la tabla de senos. `prepare()` los recrea solo si cambia la frecuencia de muestreo.
* Cambio de motor: al inicio del bloque; el motor viejo se libera y se funde en 20 ms mientras el nuevo arranca; después se resetea. Ningún objeto se crea ni se destruye en el hilo de audio.
* `process()` divide el bloque en los eventos (notas y parámetros con precisión de muestra para los motores nativos) y renderiza en trozos de ≤1024 muestras sobre búferes preasignados.
* Robo de voces común (`kit::Pool`): misma tecla → se re-golpea la voz; si no hay libre, se roba la más vieja ya liberada o, si no, la más vieja, con un fundido de ~1,3 ms antes de arrancar la nota nueva.

Tests (en `crates/stock/src/instrument/vault/tests.rs` salvo indicación):

| Test | Qué comprueba |
|---|---|
| `every_engine_plays_in_tune` | Los 7 motores dan la nota a ±6 cents (A2, A3, A4), por autocorrelación |
| `long_silent_tails_have_no_nan_or_denormals` | 12 s con resonancia y realimentación al máximo: sin NaN ni subnormales, voces apagadas, cola en cero exacto |
| `voice_stealing_keeps_the_newest_notes`, `full_polyphony_steals_without_clicks_or_overflow`, `mono_ladder_is_last_note_priority_with_legato` | Robo de voces y prioridad de nota |
| `macros_offset_the_mapped_params_only` | Rangos, profundidad negativa, curvas, y que la misma macro significa otra cosa en otro motor |
| `engine_switch_crossfades_and_releases_the_old_engine`, `wavetable_engine_is_hosted`, `sustain_pedal_defers_note_offs`, `state_round_trip` | Integración |
| `factory_presets_cover_every_engine_and_type`, `factory_presets_sit_at_a_common_loudness_without_clipping` | Biblioteca: ≥80 presets, ≥6 por motor, tipos y tags válidos, nivel −17 ± 2,5 dBFS RMS (los presets limitados por pico pueden quedar algo más bajos) y pico ≤ −1 dBFS |
| `tests/alloc.rs::vault_switches_engines_and_presets_without_rt_heap_traffic` | Cero operaciones de heap al cambiar de motor y aplicar presets de fábrica |
| `tests/alloc.rs`, `robustness.rs`, `presets.rs` (genéricos del registro) | Vault también pasa los tests de todo el registro |

Coste (release, 48 kHz, 16 voces sostenidas, `cpu_per_engine`): ladder 0,8 %, poly 9 %, FM 4 %, piano 2,4 %, órgano 4 %, cuerdas 1,9 %, wavetable 11 % de un núcleo.

---

## 4. Biblioteca de fábrica

`cargo run -p aitrack-stock --example presets_vault` genera 83 presets (`crates/stock/presets/instrument.vault/`): ladder 15, poly 17, FM 15, piano 10, órgano 10, cuerdas 8, wavetable 8, repartidos entre los 9 tipos. Cada preset guarda los parámetros globales y los del bloque de su motor (los de otros motores no suenan mientras ese motor está activo) y se escribe "con las macros a cero" más los valores iniciales de las macros. El volumen lo calibra el propio generador midiendo el preset con `vault::gain::measure` (frase fija por tipo: nota grave para bajos, nota central para leads, acorde para el resto).

---

## 5. API para la UI (tarea siguiente)

* Ids: `vault::params` (`ENGINE`, `macro_knob(m)`, `macro_depth(m)`, `VOLUME`, FX globales y los módulos `ladder`, `poly`, `fm` con `fm::op(n, campo)`, `ep`, `organ` con `organ::drawbar(n)`, `strings`; wavetable en `wt(id)`). `params::engine_of(id)` dice a qué motor pertenece un id, para mostrar solo el panel del motor activo.
* Listas: `ENGINES`, `ENGINE_TAGS`, `TYPES`, `CHARACTERS`, `FM_ALGORITHMS`, `DRAWBARS`, `VIBRATOS`, `CHORUS_MODES`…
* Macros: `macros::macros(engine)` → 4 `MacroDef { name, targets: [MacroTarget { param, from, to, curve }] }`. Valor efectivo: `Vault::effective(id)`.
* Navegador: `vault::preset_info()` → `PresetInfo { path, name, kind, engine, tags, macro_names }`. Los presets cargan con el camino normal de presets de serie (comandos).
* Meter: `[motor, voces activas, pico L, pico R, macro 1..4 (0..1), teclas pulsadas]`.
* Cambiar de motor desde la UI es un `SetParam` de `ENGINE` como cualquier otro.

Pendiente para la UI: el panel genérico de dispositivos mostraría los 559 parámetros (incluidos los del wavetable). Conviene que la vista de Vault filtre por `engine_of` y muestre las macros primero.

---

## 6. Fuentes

* A. Huovilainen, "Non-Linear Digital Implementation of the Moog Ladder Filter", DAFx-04. <https://dafx.de/paper-archive/2004/P_061.PDF>
* T. Stilson, J. O. Smith, "Analyzing the Moog VCF with Considerations for Digital Implementation", ICMC 1996.
* V. Zavalishin, *The Art of VA Filter Design* (Native Instruments, rev. 2.1). Filtros TPT/ZDF.
* V. Välimäki, A. Huovilainen, "Antialiasing Oscillators in Subtractive Synthesis", IEEE Signal Processing Magazine, 2007 (PolyBLEP).
* J. Chowning, "The Synthesis of Complex Audio Spectra by Means of Frequency Modulation", JAES 21(7), 1973.
* A. Falaize, T. Hélie, "Guaranteed-Passive Simulation of an Electro-Mechanical Piano: A Port-Hamiltonian Approach", DAFx-15. <https://www.ntnu.edu/documents/1001201110/1266017954/DAFx-15_submission_33.pdf/a87ee400-f47c-4938-92e7-4030b1338cb7> y notas: <https://afalaize.github.io/posts/rhodes>
* F. Pfeifle, "Real-time Physical Model of a Wurlitzer and Rhodes Electronic Piano", DAFx-17. <https://dafx.de/paper-archive/details/W0D0fMDJBUxiacAi_398wg>
* J. O. Smith, S. Serafin, J. Abel, D. Berners, "Doppler Simulation and the Leslie", DAFx-02. <https://ccrma.stanford.edu/~jos/doppler/dafx02.pdf>
* Sound On Sound, "Synth School" (modelos de piano eléctrico, posición de pastilla). <https://www.soundonsound.com/techniques/synth-school-part-10>
* Referencias conceptuales GPL (solo ideas, nada de código): Surge XT, Vital, Dexed, Odin 2, Helm, ZynAddSubFX.
