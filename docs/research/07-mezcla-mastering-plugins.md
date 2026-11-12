# 07 — Mezcla, mastering y plugins de referencia: estudio y plan para los dispositivos de serie

> Estudio para llevar los dispositivos de serie de AI-Track al nivel de los plugins que usa la industria.
> Fecha: 2026-11-11. Continúa a `04-plugins-dsp-fabfilter-waves.md` y no lo repite: el DSP de base (SVF, biquads matched, ADAA, FDN, true peak, sobremuestreo) está allí.
> Solo lectura: no se compiló ni se ejecutó nada. Lo que se dice de nuestro código sale de leer `crates/stock/src`, `crates/dsp/src`, `crates/engine/src/graph.rs`, `crates/model/src/project.rs` y `crates/app/src/views/device_ui`.
>
> Marcas de fiabilidad: **[P]** fuente primaria (página o manual del fabricante, norma), **[S]** fuente secundaria (reseña, blog, tienda), **[M]** dato de memoria no comprobado en esta sesión, **[NV]** no se pudo verificar. Los números sin marca dentro de una especificación propia son propuestas de diseño nuestras, no datos de terceros.

---

## 0. Resumen

1. **El hueco más caro no es un plugin, es el enrutado.** El motor llama a todos los dispositivos con `sidechain: None` (`crates/engine/src/graph.rs:91`) y el modelo no guarda una fuente de sidechain. El compresor y el gate ya tienen el parámetro "External", pero hoy no hace nada. Sin sidechain no hay ducking de bajo con bombo, que es el gesto más repetido de la música electrónica. Tampoco hay moduladores de host (LFO o envolvente arrastrables a cualquier control), que es lo que hace querido a Serum y a toda la línea FabFilter.
2. **Faltan seis dispositivos que un productor de electrónica abre en cada sesión**: curva de volumen dibujable sincronizada (clase LFOTool, Kickstart, VolumeShaper), compresor multibanda ascendente y descendente (clase OTT), clipper, de-esser, imager estéreo por bandas y supresor de resonancias (clase soothe). Ninguno es difícil salvo el último, y casi todo el DSP necesario ya está en `crates/dsp` (crossover LR4, seguidores, ADAA, sobremuestreo, true peak, LUFS).
3. **Los quince dispositivos actuales tienen buen DSP y editores visuales, pero se quedan cortos en lo que el profesional toca a diario.** Al EQ le faltan fase lineal, escucha de banda, espectro previo y posterior, entrada "2k"/"A4", selección múltiple y rango dinámico en ambos sentidos. Al limitador le faltan estilos, escucha a ganancia unitaria, escucha de la diferencia, dither con noise shaping y objetivos de loudness. El sintetizador wavetable tiene 6 tablas fijas, sin importación, sin modos de warp, 2 LFO de forma fija y 8 ranuras de matriz: está lejos de la clase Serum.
4. **Los números de plataforma que circulan son en buena parte folclore.** Solo Spotify publica cifras completas (−14 LUFS, −1 dBTP, −2 dBTP si el máster supera −14) [P]. Apple no publica un objetivo en LUFS; el −16 es una medición de terceros [S]. SoundCloud dice en su ayuda que sí normaliza [P], al contrario de lo que repiten muchos blogs. Los másters comerciales de electrónica están en torno a −8,3 LUFS integrados (rango −11,3 a −6,2) [S, iZotope 2025]. El asistente del DAW debe mostrar esos datos con su fuente y dejar los objetivos editables.
5. **Novedades de 2025–2026 que cambian la lista del usuario**: soothe3 sustituye a soothe2 (mayo de 2026) [P/S]; Ozone 12 (septiembre de 2025) añade IRC 5, Unlimiter y Stem EQ [P/S]; Serum 2 (marzo de 2025, hoy 2.1.5) pasa a 3 osciladores con 5 motores, 2 filtros, 4 envolventes, hasta 10 LFO y 8 macros [P]; Native Instruments lanzó Massive XL el 27-10-2026 como sucesor de Massive X [P]; RC-20 Retro Color 2 salió a principios de noviembre de 2026 [S]; Kontakt va por la 8 y Pigments por la 7 [P].
6. **De las referencias locales, futureboard-studio (MIT) es la más aprovechable**: tiene de-esser, ducker con curva (duckstep), clipper (67Clipper), imager de 4 bandas, limitador con medidor BS.1770, corrector de afinación (whitesharp) y un sintetizador wavetable. Se puede reescribir con diseño propio y anotar en `NOTICE` si algún bloque queda cercano. De Lemon-Studio (GPL-3) solo se toman ideas: el multibanda con mapas dibujados (Press) y el modo de fase lineal del EQ.

Orden de trabajo recomendado (detalle en §4):

| # | Qué | Valor | Esfuerzo |
|---|---|---|---|
| 1 | Enrutado de sidechain en modelo y motor | Muy alto | Medio |
| 2 | `fx.shaper`: curva de volumen dibujable (ducking sin sidechain) | Muy alto | Medio |
| 3 | `fx.clipper` | Alto | Bajo |
| 4 | `fx.multiband`: multibanda ascendente y descendente | Muy alto | Medio |
| 5 | Limitador: estilos, ganancia unitaria, diferencia, dither, objetivos | Alto | Medio |
| 6 | EQ: escucha de banda, espectro pre/post/SC, entrada numérica, rango dinámico ±, selección múltiple | Alto | Medio |
| 7 | `fx.deesser` | Alto | Bajo |
| 8 | Wavetable clase Serum, fase 1: importar tablas, warp, LFO dibujable, arrastrar para modular | Muy alto | Alto |
| 9 | `fx.imager` | Medio | Bajo |
| 10 | Asistente de mezcla y mastering (lista de comprobación, avisos, objetivos) | Alto | Medio |
| 11 | `fx.resonance`: supresor de resonancias y ducking espectral | Alto | Alto |
| 12 | `fx.color`: cinta y lo-fi | Medio | Medio |
| 13 | `fx.tune` y `fx.pitch`: afinación y formantes | Medio | Alto |
| 14 | EQ de fase lineal, EQ match, colisiones entre pistas | Medio | Alto |

---

## 1. Qué hace un ingeniero de mezcla y uno de mastering (música electrónica)

### 1.1 Mezcla, paso a paso

Cada fila dice qué se hace, con qué herramienta, qué lectura se mira y qué números son habituales. Los números son convenciones de oficio, no normas; la columna de fuente dice de dónde salen.

| # | Paso | Qué se hace | Herramienta | Lectura que se mira | Números habituales | Fuente |
|---|---|---|---|---|---|---|
| 1 | Preparación | Ordenar, nombrar, colorear, agrupar en buses (drums, bajo, sintes, voces, FX), cargar 2 o 3 temas de referencia | Grupos, pista de referencia | — | Referencias igualadas en loudness antes de comparar, en su sección más fuerte | [S] MasteringTheMix |
| 2 | Gain staging | Poner cada fuente a un nivel cómodo antes de procesar, para que los plugins con umbral y saturación trabajen en su zona | Ganancia de clip, Utility | Pico y RMS por pista | 0 VU ≈ −18 dBFS RMS (se usan también −20 y −16); picos en torno a −12 a −6 dBFS. En un DAW de coma flotante no es obligatorio técnicamente; sirve para los plugins no lineales y para no saturar el bus | [S] Sound On Sound |
| 3 | Balance estático | Mezclar solo con faders y pan. Empezar por bombo y bajo, después el resto | Faders, pan | Medidor del bus, oído | No hay convención numérica creíble para "bombo a −10 dBFS" [NV]. El método del ruido rosa (subir cada pista hasta que apenas se oiga sobre el ruido) da un balance de partida, nada más | [S] Sound On Sound |
| 4 | Relación bombo y bajo | Decidir quién ocupa el sub. Afinar el bombo a la tonalidad, separar fundamentales o alternarlas en el tiempo con ducking | Analizador, EQ, ducking | Espectro 30–120 Hz, correlación | Fundamental del bajo en house y tech house entre 40 y 80 Hz [S, blog] | [S] |
| 5 | EQ sustractiva | Quitar lo que estorba: pasa-altos en lo que no necesita graves, cortes estrechos en resonancias, limpiar 200–500 Hz | EQ paramétrico con escucha de banda | Espectro previo y posterior, escucha de banda | Cortes de 2 a 6 dB con Q de 2 a 8; pasa-altos de 12 a 24 dB/oct. No hay fuente numérica verificada para frecuencias fijas [NV] | oficio |
| 6 | EQ aditiva | Dar carácter: aire, presencia, cuerpo | EQ, shelves | Espectro, oído a volumen igualado | Realces anchos (Q 0,5 a 1,5) de 1 a 4 dB | oficio |
| 7 | Dinámica por pista | Controlar picos y dar pegada o cuerpo | Compresor, transient shaper, clipper en drums | Reducción de ganancia, historial | Voces 3 a 6 dB de reducción; drums con ataque 10 a 30 ms para dejar pasar el transitorio; clipper en bombo y caja 2 a 4 dB | [S] blogs |
| 8 | Sidechain y ducking | Bajar el bajo, los pads y las reverbs cuando suena el bombo | Compresor con sidechain, o curva de volumen sincronizada | Reducción, forma de onda de bombo y bajo superpuestas | Ataque menor de 2 a 4 ms; ratio 4:1 a 10:1 o más; release 100 a 150 ms o ajustado al tempo (negra en ms = 60000/BPM); 2 a 3 dB para un ducking discreto, 5 dB o más para que bombee | [S] EDMProd |
| 9 | Multibanda y "OTT" | Aplastar hacia arriba y hacia abajo por bandas para dar densidad y brillo a sintes, bajos y drums | Multibanda ascendente y descendente | Nivel por banda, cantidad | Mezcla (Depth) de 20 a 50 % en la mayoría de los casos; 100 % en growls y leads agresivos | oficio |
| 10 | Saturación | Añadir armónicos para que el sub se oiga en altavoces pequeños y los sintes llenen | Saturador, cinta, distorsión multibanda | Curva de transferencia, armónicos, nivel igualado | Comparar siempre a ganancia compensada | oficio |
| 11 | Resonancias y sibilancia | Domar asperezas que cambian de frecuencia (voces, platos, supersaws) | De-esser, supresor de resonancias, EQ dinámico | Reducción por frecuencia, escucha de la diferencia | Sibilancia en 4 a 10 kHz; guía de Logic: 5 a 8 kHz voz femenina, 3 a 6 kHz masculina [P, Apple]; FabFilter cita 8 a 10 kHz como típico [P] | [P] |
| 12 | Espacio en envíos | Reverbs y delays en retornos compartidos, filtrados y con ducking | Envíos, reverb, delay, EQ en el retorno | Nivel del retorno, espectro | Truco de Abbey Road: pasa-altos a 600 Hz y pasa-bajos a 10 kHz antes de la reverb [S]. Pre-delay muy variable (8 a 120 ms según la fuente) [NV como regla] | [S] Soundfly |
| 13 | Estéreo y mono | Ensanchar medios y agudos, mono en graves, comprobar la suma a mono | Utility, imager, vectorscopio | Correlación, balance | Mono por debajo de 80 a 150 Hz; correlación por encima de 0. Es un criterio por defecto, no una regla | [S] Sonible |
| 14 | Buses | Pegar los grupos con compresión suave, EQ de bus, saturación de bus | Compresor de bus, EQ | Reducción | 1 a 3 dB de reducción, ratio 2:1 a 4:1, ataque 10 a 30 ms, release automático | [S] |
| 15 | Automatización | Mover faders, filtros y envíos para dar forma a la canción: subidas, cortes antes del drop, aperturas de filtro | Automatización, envolventes de clip | — | Bajar 0,5 a 1 dB antes del drop hace que el drop parezca más fuerte | oficio |
| 16 | Comprobaciones | Mono, volumen bajo, auriculares, altavoz pequeño, comparar con referencias a loudness igualado | Analizador, medidor de loudness, pista de referencia | LUFS, espectro frente a la referencia, correlación | Monitorización calibrada: ruido rosa a −20 dBFS RMS por altavoz a 74–83 dB SPL (C, lento) según el volumen de la sala | [S] Sound On Sound |
| 17 | Margen para el máster | Exportar sin limitador en el bus y sin clipping | Medidor | Pico real, sin "overs" | No hay norma. Se pide −6 dBFS de pico, o simplemente que no recorte y que no lleve limitador [S]. En 24 bits o coma flotante el nivel exacto no afecta a la calidad | [S] Splice |

Pendiente espectral de referencia: el ruido rosa cae 3 dB/oct; SPAN y Pro-Q dibujan por defecto con 4,5 dB/oct de inclinación [P]; un estudio sobre 772 números uno encontró unos 5 dB/oct de caída entre 100 Hz y 4 kHz [S, Pestana et al. 2013]. Un espectro que se ve plano con 4,5 dB/oct de inclinación es una mezcla equilibrada.

Cadena de voz habitual: (puerta) → EQ sustractiva → compresor → de-esser → EQ aditiva → saturación → reverb y delay por envío. La posición del de-esser se discute [S, iZotope].

### 1.2 Mastering, paso a paso

Orden de cadena. La guía de iZotope pone EQ correctiva → dinámica multibanda o correctiva → compresión de banda ancha → EQ tonal → limitador, y dice que EQ, compresor y limitador suelen bastar [S]. El orden largo (saturación → imagen → clipper → limitador → dither) es consenso de blogs, no una norma.

| # | Etapa | Qué se decide | Herramienta | Lectura | Números habituales | Fuente |
|---|---|---|---|---|---|---|
| 1 | Escucha y medición | Comparar con referencias a loudness igualado; apuntar problemas | Medidor, analizador, referencia | LUFS integrado y a corto plazo, espectro promedio, correlación, pico real | — | oficio |
| 2 | EQ correctiva | Quitar subgrave inútil, resonancias, dureza | EQ (a menudo M/S y fase lineal) | Espectro promedio frente a la referencia | Movimientos de 0,5 a 2 dB con Q ancha [NV: la guía de iZotope no da cifras] | oficio |
| 3 | Dinámica correctiva | Controlar una banda que salta (graves sueltos, platos) | Multibanda o EQ dinámico | Reducción por banda | Ejemplo de iZotope: bandas por debajo de unos 130 Hz y por encima de 3 kHz | [S] iZotope |
| 4 | Compresión de bus | Pegar la mezcla | Compresor estilo VCA de bus | Reducción | Ratio 1,25:1 a 2:1; 1 a 3 dB de reducción (Splice: 2 dB como máximo); ataque discutido (unos 30 ms o más lento); release automático o al tempo | [S] Splice |
| 5 | EQ tonal | Aire, peso, presencia | EQ, shelves anchos | Espectro | ±0,5 a 1,5 dB | oficio |
| 6 | Saturación o excitador | Densidad y armónicos | Saturador, cinta | Nivel igualado | Poco; se juzga con ganancia compensada | oficio |
| 7 | Imagen | Mono en graves, ancho en agudos | Imager por bandas, EQ M/S | Correlación, vectorscopio | Graves en mono por debajo de 100 a 150 Hz, sobre todo si va a vinilo | [S] |
| 8 | Clipper | Recortar los picos de bombo y caja para que el limitador trabaje menos | Clipper con sobremuestreo | dB recortados | 1 a 2 dB en el máster; 2 a 4 dB en drums sueltos. Sin fuente de revista; es práctica de blogs y de fabricantes | [S] Newfangled |
| 9 | Limitador | Subir al loudness objetivo sin distorsión audible | Limitador de pico real | Reducción, LUFS a corto plazo, pico real | 1 a 3 dB de reducción suena transparente; los másters fuertes necesitan más. Sobremuestreo 4× como mínimo; FabFilter recomienda 4× con al menos 0,1 ms de lookahead [P] | [P] FabFilter, [S] |
| 10 | Techo | Dejar margen para los códecs con pérdida | Limitador | dBTP | −1 dBTP (Spotify, AES, EBU, Apple pide "al menos 1 dB"); −2 dBTP si el máster es fuerte (Spotify, SoundCloud). FabFilter lista −0,1 dBTP para CD. En la práctica la mayoría de los éxitos de 2024 pasan de 0 dBTP | [P], [S] |
| 11 | Dither | Reducir la profundidad de bits | Dither con noise shaping opcional | — | Una sola vez, al final, solo al bajar de bits; no para MP3 o AAC [P, FabFilter]. Conversión de frecuencia de muestreo antes del dither | [P], [S] |
| 12 | Control de calidad | Escuchar el archivo final entero; comprobar inicio y final, clics, metadatos | Medidor, reproductor | LUFS, LRA, pico real del archivo | — | oficio |

### 1.3 Objetivos por plataforma (lo que está publicado y lo que no)

| Plataforma | Lo que publica | Lo que es medición de terceros |
|---|---|---|
| Spotify | −14 LUFS según ITU 1770; niveles Loud −11, Normal −14, Quiet −19 (Premium). Sube los másters bajos dejando 1 dB de margen; en Loud aplica un limitador a −1 dB con 5 ms de ataque y 100 ms de caída. Recomienda −1 dBTP, o −2 dBTP si el máster supera −14 LUFS. El reproductor web no normaliza [P] | — |
| Apple Music | Sound Check sube o baja por pista o por álbum. Apple Digital Masters: 24 bits a la frecuencia nativa (44,1 a 192 kHz), "al menos 1 dB de margen", comprobar el recorte con `afclip` [P] | −16 LUFS [S, Ian Shepherd] |
| YouTube | No publica número. "Volumen estable" no se aplica a música oficial [P] | −14 LUFS, solo baja; lectura "content loudness" en Estadísticas para nerds [S] |
| Tidal | Nada oficial encontrado | −14 LUFS, por álbum, solo baja [S] |
| Amazon Music | Solo un interruptor [P] | −14 LUFS [S]; el "−2 dBTP" no se pudo rastrear hasta Amazon [NV] |
| Deezer | Solo un interruptor [P] | −15 LUFS [S] |
| SoundCloud | Dice que normaliza y da el mismo consejo que Spotify (−14 LUFS, −1 dBTP). Subir sin pérdida, 16 bits y 44,1 kHz o más [P] | Muchos blogs dicen que no normaliza: contradice la ayuda oficial |
| Beatport | Estéreo, 44,1 kHz y 16 bits (página de 2023) [P]. Vende descargas: no normaliza | 24 bits y 48 kHz admitidos según una fuente secundaria [NV] |
| Bandcamp | WAV, AIFF o FLAC, 16 bits y 44,1 kHz como mínimo; 24 bits sí, 32 no [P] | "No normaliza" [S] |
| Club (CDJ, rekordbox) | El Auto Gain de rekordbox no viaja al USB: en cabina suena el nivel del archivo más el trim del DJ [P, manual] | Objetivo de Auto Gain sin documentar; un fabricante midió unos −10 LUFS [S] |
| TikTok, Instagram | Nada oficial | Cifras contradictorias [NV] |

Normas de contexto: ITU-R BS.1770-5 (noviembre de 2023) define la ponderación K, bloques de 400 ms con 75 % de solape, puerta absoluta a −70 LKFS y relativa a −10 LU, y pico real con sobremuestreo 4× [P]. EBU R 128 v5.0: −23 LUFS, máximo −1 dBTP [P]. ATSC A/85: −24 LKFS, pico real por debajo de −2 dBTP [P, valores por resúmenes]. AES TD1008 (2021, luego AES77-2023): música a −16 LUFS con normalización por pista, pista más fuerte del álbum a −14 con normalización por álbum, máximo −1 dBTP a la entrada del códec [P].

Consecuencia para el diseño: los objetivos del asistente son presets editables con su fecha y su fuente, no constantes en el código.

### 1.4 Loudness comercial por género

Las fuentes de más confianza son los dos estudios de iZotope y los ingenieros con nombre. Las tablas por género vienen de vendedores de herramientas de medición, con métodos que no se pueden auditar, y discrepan entre sí de 2 a 4 LU. Las que miden previsualizaciones de 30 a 120 s miden la parte fuerte, no el tema entero.

Anclas:
- 54 canciones del top 10 del Billboard Global 200, primera mitad de 2024: integrado −8,3 ± 1 LUFS (de −11,1 a −6,0), máximo a corto plazo −6 ± 1, LRA entre 3,7 y 6,8 LU; la mitad con pico real entre 0 y +0,75 dBTP [S, iZotope].
- Hot Dance/Electronic 2025: integrado medio −8,3 (de −11,3 a −6,2), corto plazo medio −6,1, LRA 5,5 a 6,6 [S, iZotope].
- MasteringTheMix: "fuerte" es unos −6 LUFS a corto plazo en la sección más fuerte; "dinámico" es −9 a −11. Para club, −6 a −9 LUFS con −0,1 dBTP [S].
- Ian Shepherd: no pasar de −10 LUFS a corto plazo con pico real de −1 como máximo [S].

| Género | Integrado, mediana según fuentes (LUFS) | Fiabilidad |
|---|---|---|
| House | −9,2 a −8,0 | Vendedores [S] |
| Tech house | −8,9 a −8,8 | [S] |
| Techno peak-time | −8,6 a −6,5 | [S], discrepan 2 LU |
| Techno melódico | −10,5 a −8,8 | [S] |
| Hard techno | −7,3 a −6,7 | [S] |
| Drum & bass | −6,5 a −4,3 | [S], discrepan 2 LU |
| Dubstep | −6 a −4,5 | [S] |
| EDM, big room | −8,7 a −6,7 | [S] |
| Trap, hip-hop | −10,0 a −8,5 | [S] |
| Pop | −9,5 a −8,3 | iZotope + [S] |
| Latino (no específico de reggaetón) | −10,7 a −9,4 | [S], muestra pequeña |
| Reggaetón | Sin medición creíble; una guía aconseja −9 a −7 | [NV] |
| Ambient | −14,1 a −13,8 | [S] |
| Lo-fi | Sin dato creíble | [NV] |

Lectura práctica: para streaming, cualquier máster por encima de −14 LUFS se baja; lo que se gana al masterizar fuerte es densidad, no volumen. Para club y descarga no hay normalización y el nivel del archivo cuenta. Por eso el género manda más que la plataforma.

### 1.5 Entregables

- Máster principal: WAV de 24 bits a la frecuencia de la sesión (Spotify acepta FLAC o WAV de 44,1 kHz y 16 bits o más y reproduce como máximo 24 bits y 44,1 kHz [P]).
- Para CD o DDP: 16 bits y 44,1 kHz con dither [M].
- Apple Digital Masters: 24 bits, frecuencia nativa, comprobado contra el codificador AAC [P].
- Versiones: instrumental, radio edit, clean, extended o club mix; stems si el sello los pide.
- Vinilo: graves en mono por debajo de 100 a 150 Hz (algunos cortadores piden no hacerlo salvo que lo pidan), sibilancia controlada; un 12" a 33⅓ rpm suena mejor con 15 a 18 minutos por cara, a 45 rpm con 9 a 12 [S, Disc Makers].
- Álbum: no forzar todas las pistas al mismo LUFS; con normalización por álbum la pista más fuerte suele quedar unas 2 LU por encima de la media [P, AES].
- Metadatos: ISRC, título, artista, y el informe de loudness (integrado, LRA, pico real) junto al archivo.

Nuestro diálogo de exportación (`crates/app/src/views/export.rs`) ya ofrece WAV de 16 bits con dither, 24 bits y 32 bits flotante, MP3 a 320 kbps, stems y normalización de pico a −0,3 dBFS. Le faltan: normalizar a un LUFS objetivo, techo en dBTP, informe de loudness del archivo y perfiles por destino (§5).

---

## 2. Los plugins de referencia en 2025–2026, por categoría

Para cada uno: versión comprobada y lo que lo hace querido, en términos de funciones e interacciones. Las URL están en §7.

### 2.1 EQ y control espectral

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| FabFilter Pro-Q 4 | La pantalla es el control. Bandas dinámicas y espectrales, lista de instancias, EQ match, agarrar picos del espectro, colisiones entre pistas. Detalle en §3.1 | [P] |
| Three-Body Kirchhoff-EQ (1.7.4, julio de 2026) | 32 bandas, 15 tipos de filtro, dinámica con dos umbrales (por encima y por debajo), detección relativa, lookahead, modos de fase cero latencia, analógico, lineal y mixto, modelos de EQ clásicos. Es el rival "más técnico" de Pro-Q | [P] página de Plugin Alliance; el número de modelos vintage es incoherente en la propia página |
| oeksound soothe3 (1.0.6; lanzado el 19-05-2026) | Sustituye a soothe2. Supresión de resonancias que sigue a la señal: no hay que buscar la frecuencia. Un control Detail en lugar de Sharpness y Selectivity, Tilt, Max cut, modos Soft (umbral adaptativo) y Hard, hasta 8 bandas de ponderación, L/R o M/S, sidechain externo con escucha, delta. Latencia 2304 muestras (0 en baja latencia, 3456 en fase lineal). Cortes de hasta 40 dB | [P] manual |
| oeksound soothe2 (anterior) | Depth, Sharpness, Selectivity, Attack, Release, Mix, Trim, Delta, Soft/Hard; 4 bandas más cortes | [P] |
| oeksound spiff (1.4.4), bloom (1.1.x) | spiff: transitorios por análisis espectral, sin crossovers. bloom: ecualizador tonal adaptativo con 4 controles | [P] |
| Soundtheory Gullfoss (Standard, Live, Master) | EQ que cambia "más de 300 veces por segundo" según un modelo auditivo. Cinco controles: Recover (sube lo enmascarado), Tame (baja lo que enmascara), Bias, Brighten, Boost. Marcadores de rango arrastrables | [P] ediciones y latencias; [S] controles |
| TDR Nova (2.2.3, gratis) | 4 bandas dinámicas más HP/LP, con sección de dinámica completa por banda. La alternativa gratuita de EQ dinámico | [P] |
| Wavesfactory Trackspacer (2.5.10) | EQ de 32 bandas que aplica la curva inversa del espectro del sidechain: abre hueco para la voz o el bombo sin tocar el resto. Un control (Amount) más filtros, ataque, release y M/S | [P] |

Lo que piden estos plugins a un DAW: buen analizador, bandas dinámicas en ambos sentidos, sidechain por banda, un supresor espectral y ducking espectral por sidechain.

### 2.2 Dinámica

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| FabFilter Pro-C 3 | 14 estilos (Clean, Versatile, Smooth, Punch, Upward, TTM; Op-El, Vari-Mu, Classic, Opto; Vocal, Mastering, Bus, Pumping). Carácter Tube, Diode o Bright. Umbral automático, disparo por tempo del host, EQ de sidechain de 6 bandas, lookahead 0–20 ms, hold hasta 500 ms, mix 0–200 %, sobremuestreo hasta 32× | [P] |
| FabFilter Pro-MB | Hasta 6 bandas colocadas libremente, pendientes 6–48 dB/oct, modos Dynamic Phase, Linear Phase y Minimum Phase, compresión o expansión por banda, sidechain externo, mix 0–200 % | [P] |
| Xfer OTT (1.37, gratis) | El sonido de la electrónica desde 2010: tres bandas con compresión descendente y ascendente a la vez. Controles: Depth, Time, ganancia de entrada y salida, ganancia por banda, Upward % y Downward %. Cruces fijos en 88,3 Hz y 2,5 kHz. Viene del preset "OTT" de Multiband Dynamics de Ableton | [P] versión; [S] controles y cruces |
| Ableton Multiband Dynamics | Tres bandas, cada una con umbral "Above" y "Below"; cada bloque puede comprimir o expandir. Time escala todos los ataques y releases; Amount escala todas las ratios; Soft Knee; RMS o Peak; sidechain | [P] manual de Live 12 |
| SSL Native Bus Compressor 2 | El compresor de bus VCA de la consola 4000 G. Ratios clásicas 2, 4 y 10 más 1,5, 3, 20 y X; ataque nuevo de 20 ms; releases nuevos de 0,4, 0,8 y 1,2 s; HPF de sidechain, Mix, sobremuestreo | [P] |
| Cytomic The Glue (1.9.3) | La misma idea con Range (límite de reducción), HPF de sidechain de 6 dB/oct, Mix y Peak Clip | [P] |
| Ableton Glue Compressor | Range, Dry/Wet, Soft Clip a −0,5 dB, sobremuestreo 2× | [P] |
| Xfer LFOTool (1.5 según el enlace de demo) | Curva de LFO dibujable para volumen, pan y filtro; 12 gráficas por preset conmutables por nota MIDI; sincronía exacta a la muestra | [P] parcial |
| Kickstart 2 (2.0.9) | Ducking en un clic: 16 curvas, arrastrar para ajustar la longitud, disparo por audio que sigue cualquier patrón de bombo, modo multibanda que solo baja los graves, vista de bombo y bajo superpuestos | [P] |
| Cableguys ShaperBox 3 (3.6) y VolumeShaper 7 | 11 Shapers (Pitch, Reverb, Time, Drive, Noise, Liquid, Filter, Crush, Volume, Pan, Width), 3 bandas por Shaper, cada una con su LFO dibujable y su seguidor de envolvente; disparo por transitorios de audio, incluido el sidechain externo; longitudes de 1/128 a 32 compases | [P] |

### 2.3 Limitadores y clippers

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| FabFilter Pro-L 2 | 8 estilos, pico real, medidor de loudness con objetivos, escucha a ganancia unitaria y de la diferencia. Detalle en §3.2 | [P] |
| iZotope Ozone 12 (septiembre de 2025) | Maximizer con IRC 5, Upward Compress y Soft Clip. Módulos nuevos: Stem EQ, Unlimiter, Bass Control. Master Assistant con flujos a medida, objetivo de loudness y hasta 60 s de análisis | [P/S]; la lista completa de modos IRC no se pudo leer en la ayuda [NV] |
| SIR StandardCLIP | Clipper de referencia en electrónica. Modos Hard Clip, Soft Clip Classic y Soft Clip Pro (al 25/50/75/100 % comprime los últimos 6/12/18/24 dB a la mitad). Sobremuestreo hasta 256× con filtros de fase lineal o mínima, factor distinto para render | [P] manual |
| Kazrog KClip 3 | 8 modos (Smooth, Crisp, Tube, Tape, Germanium, Silicon, Broken Speaker, Guitar Amp), 4 bandas con modo propio, M/S, medidor LUFS, sobremuestreo hasta 32× | [P] |
| Schwabe Digital Gold Clip | Clippers Modern, Classic y Hard; "Gold" añade hasta 6 dB de ganancia no lineal que desaparece cerca del techo; Alchemy contornea medios y agudos después del recorte; mezcla paralela | [P] |
| Newfangled Elevate (1.5) | Limitador de 26 bandas en escala Mel con limitador adaptativo por banda, énfasis de transitorios y clipper espectral | [P] |

### 2.4 De-essers, saturación y color

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| FabFilter Pro-DS | Detección Single Vocal o Allround, banda ancha o banda partida, lookahead hasta 15 ms. Detalle en §3.3 | [P] |
| FabFilter Saturn 2 | 28 estilos, hasta 6 bandas, drive, mix, feedback, dynamics, tone y nivel por banda, HQ 8× y 32×, 50 ranuras de modulación | [P] |
| Soundtoys Decapitator (5.5) | Cinco estilos (A, E, N, T, P) modelados sobre previos y distorsionadores de estudio, Punish (+20 dB), Tone, filtros, Mix y compensación automática de salida | [P] producto; estilos por copia del manual |
| XLN RC-20 Retro Color 2 (noviembre de 2026) | Seis módulos reordenables (Noise, Wobble, Distort, Digital, Space y el nuevo Echo), Magnitude que escala todo, Varispeed, sincronía de tempo, compresor y clip maestros. La v1 tenía Magnetic y el deslizador Flux, que la página actual ya no muestra | [P] v2; v1 [M] |
| Dada Life Sausage Fattener | Dos controles (Fatness, Color) y ganancia: compresión más saturación en un gesto | [S]; sin página oficial encontrada |
| iZotope Trash (relanzado) | Más de 60 distorsiones y más de 600 respuestas a impulso, mezcladas en una rejilla XY; 3 bandas | [P] |
| Soundtoys Devil-Loc Deluxe | Compresor y distorsión extrema de dos controles para drums | [P] |

### 2.5 Reverb y delay

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| Valhalla Room (2.0.5) | 12 algoritmos, secciones Early y Late separadas, decay de 0,1 a 100 s con tres bandas | [P] |
| Valhalla VintageVerb (4.0.5) | 22 modos y tres "colores" de época (1970s, 1980s, Now) que cambian el ancho de banda y la modulación | [P] |
| Valhalla Supermassive (5.0.0, gratis) | 22 modos sobre redes de delays con realimentación de hasta 2 s por línea; Warp convierte ecos en reverb; colas enormes | [P] |
| ValhallaDelay (3.0.0) | Modos Tape, HiFi, BBD, Digital, Ghost, Pitch, RevPitch; Era y Age cambian el modelo y su desgaste | [P] |
| FabFilter Pro-R 2 (2.06) | Control Space continuo, EQ de tiempo de decay de 6 bandas, post-EQ, ducking, auto gate, congelación, importar IR | [P] |
| Soundtoys EchoBoy (5.5) | 30 estilos de eco, modos Single, Dual, Ping-Pong y Rhythm Echo (hasta 16 taps), Groove y Feel para que las repeticiones adelanten, retrasen o balanceen | [P] |
| Waves H-Delay | 1 a 3500 ms, cuatro caracteres analógicos, botón LoFi, HP/LP enlazables, modulación, ping-pong | [P] |

### 2.6 Estéreo, utilidades y medición

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| iZotope Ozone Imager (gratis) e Imager de Ozone 12 | Ancho por bandas (hasta 4), Stereoize I (decorrelación tipo Haas) y II, tres vectorscopios (Polar Sample, Polar Level, Lissajous), correlación, "Recover Sides" en Ozone 12 | [P] |
| Voxengo MSED (3.13, gratis) | Codificar y decodificar M/S, ganancia y pan de Mid y Side, mute de cada uno, correlación | [P] |
| Voxengo SPAN (3.24, gratis) | Analizador con tamaño de bloque, solape y pendiente ajustables (4,5 dB/oct por defecto), M/S, correlación, escalas K, LUFS, pico real | [P] |
| Youlean Loudness Meter 2 (2.5.15) | LUFS integrado, corto y momentáneo, LRA, PLR, DR, pico real, gráfica de historial e histograma; presets de plataformas en la versión Pro | [P] |
| ADPTR Metric AB, Mastering The Mix REFERENCE y LEVELS | Comparar con hasta 16 referencias a loudness igualado; LEVELS marca en rojo la sección que se sale de su umbral | [P/S] |
| Sonarworks SoundID Reference | Corrección de auriculares y monitores | [P] |

### 2.7 Afinación y voz

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| Antares AutoTune Pro 11 (11.6.0) | Modo Auto en tiempo real y modo Graph por nota; Retune Speed, Flex Tune, Humanize, Transpose, baja latencia; Harmony Player de 4 voces; ARA2; modo clásico "AutoTune 5" | [P]; Throat y Formant en la 11 [S] |
| Celemony Melodyne 5 | Edición por nota de afinación, tiempo, vibrato, formantes y sibilantes; polifónico (DNA) en las ediciones editor y studio; ARA. No hay Melodyne 6 | [P] |
| Soundtoys Little AlterBoy (5.5) | Pitch ±12 semitonos, Formant, Link, modos Transpose, Quantize y Robot, Drive | [P] |
| Ableton Auto Shift (Live 12) | Corrección en tiempo real con vibrato, desplazamiento de tono y de formantes, y modo MIDI | [P] manual |

### 2.8 Sintetizadores

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| Xfer Serum 2 (2.1.5) | Detalle en §3.4 | [P] |
| Vital | Wavetable con warp espectral, modulación arrastrable con vista animada, LFO dibujables, texto a wavetable. Código GPLv3: no se copia | [P]; versión actual [NV] |
| NI Massive X y Massive XL (27-10-2026) | 2 osciladores wavetable con diez modos (Standard, Bend, Mirror, Hardsync, Wrap, Formant, ART, Gorilla, Random, Jitter), enrutado libre, Performers, 16 macros. Massive XL añade importación de wavetables y generador de presets desde audio | [P] |
| Arturia Pigments 7 (7.0.1) | Seis motores (Wavetable, Virtual Analog, Sample, Granular, Harmonic, Modal), modulación muy visual, 20 efectos en dos buses de inserción y un envío | [P] |
| u-he Diva (1.4.8) | Osciladores y filtros analógicos modelados con realimentación sin retardo, mezclables entre modelos; calidad draft, fast, great y divine | [P] |
| LennarDigital Sylenth1 (3.072) | 4 osciladores con 8 voces de unísono cada uno; poco CPU y un sonido que encaja en la mezcla sin trabajo | [P] |
| Kilohearts Phase Plant | Semimodular: generadores de onda, wavetable, sample y ruido, y tres carriles de efectos | [P] |

### 2.9 Samplers, baterías y efectos creativos

| Plugin (versión) | Por qué se usa | Marca |
|---|---|---|
| NI Kontakt 8 | El estándar de instrumentos muestreados; la 8 añade Tools (Chords, Phrases, Patterns), Leap para loops y osciladores wavetable | [P] |
| NI Battery 4 | Matriz de celdas (hasta 128), efectos por celda, buses, arrastrar para enrutar | [P] |
| XLN XO (1.8.6) | Ordena los one-shots por parecido sonoro en un mapa ("Space"), secuenciador con Accentuator, combinador de ritmos y de samples | [P] |
| Output Portal | Efecto granular sobre la entrada con un control XY | [S] |
| Sugar Bytes Effectrix 2 (2.0.4) | 14 efectos secuenciados en una rejilla de 32 pasos | [P] |
| Image-Line Gross Beat | Búfer de 2 compases con 36 curvas de tiempo y 36 de volumen dibujables: half-time, scratch, gate | [P] manual; venta como VST sin confirmar [NV] |
| Cableguys HalfTime (1.1.x) | Medio tiempo en un botón: modos 2×, 1,5× y 4×, bucle de 1/16 a 8 compases, banda partida, fundidos de entrada y salida | [P] |

Los dispositivos de Live que el usuario ya conoce y con los que nos comparará: EQ Eight, Compressor, Glue Compressor, Multiband Dynamics, Limiter (modos Standard, Soft Clip y True Peak; lookahead de 1,5, 3 o 6 ms; Maximize; L/R o M/S) [P], Saturator, Roar, Auto Filter, Auto Pan-Tremolo, Echo, Drum Buss, Shifter, Spectral Resonator, Spectral Time, Vocoder, Utility, Wavetable, Operator, Drift, Meld, Simpler, Sampler y Drum Rack.

---

## 3. Los cuatro que nombró el usuario, como especificación

Cada apartado tiene dos partes: **lo que hace el original** (verificado en su ayuda, con marca) y **nuestra especificación** (diseño propio: otros nombres, otra disposición, otro aspecto). No se copian nombres de modos, iconos, colores ni la forma de los paneles.

### 3.1 EQ de clase Pro-Q

#### Lo que hace Pro-Q 4 [P, ayuda de FabFilter]

**Bandas y formas**
- Hasta 24 bandas [P, doc 04]. Diez formas: Bell, Low Shelf, Low Cut, High Shelf, High Cut, Notch, Band Pass, Tilt Shelf, Flat Tilt, All Pass.
- Pendientes de 0 a 96 dB/oct, con valores fraccionarios; Low Cut y High Cut ofrecen además Brickwall. Mínimos: Bell y Notch 12 dB/oct; cortes y Band Pass 0; el resto 6.
- Frecuencia de 10 Hz a 30 kHz. Ganancia de −30 a +30 dB. Q sin rango publicado en la página; 1 es el ancho por defecto.
- Botón de interacción ganancia-Q: en las campanas la Q se estrecha al subir la ganancia.
- Colocación estéreo por banda (Stereo, L, R, Mid, Side) y botón de tijera que parte una banda en L/R o M/S.

**Crear bandas**
- Arrastrar la curva amarilla global crea una banda, con vista previa de la forma mientras se arrastra.
- Pasar el ratón por el fondo muestra una vista previa; clic o doble clic la añade.
- Doble clic en la zona baja: Notch. Doble clic en el extremo izquierdo o derecho: Low Cut o High Cut. Arrastrar la curva en los extremos: shelf.
- Alt+clic crea una banda dinámica; Alt+Shift+clic, una espectral.
- EQ Sketch: dibujar la curva entera de un trazo y el EQ propone las bandas.

**Editar**
- Arrastrar el punto: frecuencia y ganancia. Con varias bandas seleccionadas, la ganancia escala de forma relativa.
- Rueda: Q; en cortes, la pendiente por pasos (Shift da pendientes fraccionarias). Ctrl/Cmd+rueda: ganancia. Alt+rueda: rango dinámico. Alt+Ctrl/Cmd+rueda: cambia ganancia y rango dinámico enlazados en sentido contrario.
- Ctrl/Cmd+arrastre vertical: Q de todas las seleccionadas. Shift: ajuste fino. Alt al arrastrar: bloquea un eje.
- Alt+clic en el punto: bypass. Ctrl+Alt+clic: cambia la forma. Alt+Shift+clic: cambia la pendiente.
- Doble clic en el punto: entrada numérica; Tab recorre frecuencia, ganancia y Q. La frecuencia admite "100", "2k", "A4" o "C#2+13".
- Selección: clic, rectángulo sobre el fondo, Ctrl/Cmd+clic para sumar, Shift+clic para un rango.
- Clic derecho en el punto: menú de banda (copiar, hacer dinámica, hacer espectral…). Clic derecho en el fondo: pegar. Las bandas se copian entre instancias.
- Panel de valores flotante junto al punto con bypass, borrar, solo y forma.

**Pantalla**
- Escala de EQ de ±3, 6, 12 o 30 dB; se amplía sola si se arrastra más allá. Zoom horizontal arrastrando la escala de frecuencias; doble clic vuelve al rango completo. La pantalla llega a 30 kHz.
- Teclado de piano opcional en el eje de frecuencias, para cuantizar a notas.
- Pantalla completa y reescalado.

**EQ dinámico**
- Campanas y shelves (y Flat Tilt, según la sección de controles) pueden ser dinámicas, también en fase lineal hasta la resolución High.
- Rango dinámico de −30 a +30 dB: negativo comprime, positivo expande. Se ajusta con un anillo alrededor del control de ganancia o arrastrando el indicador en la pantalla.
- Modo automático por defecto: ataque y release automáticos y umbral que sigue el nivel de la banda. Un panel "expandir" da umbral manual (con el nivel de disparo dibujado en el deslizador), ataque y release en una escala porcentual (50 % = automático), sidechain externo, disparo por la banda o "Free" con filtros propios, y escucha del disparo.
- Una barra amarilla en el anillo enseña el cambio de ganancia en vivo.

**Dinámica espectral**
- Variante que solo actúa sobre las frecuencias de la banda que superan el umbral. Controles añadidos: Spectral Density (ancho de las zonas disparadas) y Spectral Tilt (3 dB/oct sobre el espectro de detección, activado por defecto). Obliga a fase lineal en esa banda.

**Modos de proceso y carácter**
- Zero Latency, Natural Phase y Linear Phase (resoluciones Low a Maximum; latencias en doc 04 §1.2). Carácter Clean, Subtle y Warm.

**Analizador**
- Pre, Post y SC/Ext (sidechain externo o el espectro posterior de otra instancia, dibujado con contorno rojo claro).
- Rango 60, 90 (por defecto) o 120 dB. Resolución 1024, 2048, 4096 u 8192 puntos. Velocidad de caída. Tilt con 4,5 dB/oct por defecto. Freeze (mantenido o fijo).
- Colisiones: resplandor rojo donde el espectro propio y el externo pueden enmascararse.
- Spectrum Grab: dejar el ratón quieto sobre el espectro unos segundos lo congela, etiqueta los picos principales con su frecuencia (o su nota) y permite arrastrar un pico para crear una campana con Q automática.
- Frecuencia bajo el cursor en la escala inferior.

**EQ Match**
- Referencia: espectro guardado, la propia entrada en otro pasaje, otra instancia, el sidechain o un archivo de audio arrastrado.
- Los espectros se promedian; tras unos 30 s la media se estabiliza y una línea blanca muestra la diferencia. "Match" propone bandas; un deslizador elige cuántas (más bandas siguen diferencias pequeñas, menos solo la forma general).

**Salida**
- Auto Gain (estimación estática a partir de las curvas, no medida), Gain Scale (escala la ganancia de todas las campanas y shelves), nivel de −∞ a +36 dB, pan L/R o M/S, inversión de fase, bypass con compensación y sin clics.
- Lista de instancias: todas las instancias del proyecto en una vista, editables desde cualquier ventana.

#### Nuestra especificación (`fx.eq`, evolución del actual)

Estado actual: 8 bandas; Bell, Low Shelf, High Shelf, Low Cut, High Cut, Notch, Band Pass, Tilt; 6–96 dB/oct; canal Stereo/Mid/Side/Left/Right; dinámica solo descendente (umbral, ratio, ataque, release); modos Matched y Classic; auto gain; editor con nodos, rueda para Q, doble clic para crear según la zona, panel de valores, menú contextual, flechas y `[` `]`.

| Área | Especificación | Notas de DSP |
|---|---|---|
| Bandas | 16 en v1.1 (24 después). Formas nuevas: tilt plano, all-pass, corte "muro" | All-pass de 2.º orden con SVF TPT. Tilt plano: cascada de shelves de primer orden repartidos por octavas. Corte "muro": elíptico de orden alto o FIR en fase lineal |
| Pendientes | Las 8 actuales más fraccionarias | Mezcla de la salida de dos órdenes vecinos (doc 04 §6.1) |
| Rango | 10 Hz – 30 kHz, ±30 dB, Q 0,025 – 40 | Hoy 20 Hz – 20 kHz, ±24 dB, Q 0,1 – 18 |
| Dinámica por banda | Sustituir umbral+ratio por **Rango dinámico** −30…+30 dB (negativo comprime, positivo expande) más umbral. Umbral automático por defecto; ataque y release automáticos con un control de velocidad 0–100 %. Disparo: la banda, filtro libre o sidechain externo. Escucha del disparo | El detector ya existe. Umbral automático: media móvil del nivel de la banda (constante de 1 a 3 s) más un margen. Requiere el sidechain del motor (§4.0) |
| Modo espectral | En v2, dentro de la banda: solo bajan los bins que sobresalen | Reutiliza el núcleo STFT de `fx.resonance` (§4.3.5) limitado al rango de la banda |
| Proceso | Directo (SVF), Analógico (matched) y Fase lineal (2048, 4096, 8192, 16384) | FIR por muestreo en frecuencia y convolución particionada; crossfade al cambiar (doc 04 §3.2). Lemon-Studio tiene un modo así para estudiar la idea, sin copiar |
| Analizador | Capas Pre, Post y Sidechain u otra pista; rango 60/90/120 dB; resolución; caída; inclinación 0/3/4,5 dB/oct; congelar; media larga | `SpectrumAnalyzer` de `crates/dsp/src/spectrum.rs` ya da el posterior. El previo sale de un segundo tap. El de otra pista lo da el bus de análisis del host |
| Escucha de banda | Mantener pulsado un nodo con una tecla (por ejemplo, Alt+clic mantenido) deja oír solo lo que esa banda toca: pasa-banda con la misma frecuencia y Q; en shelves y cortes, la zona afectada | Parámetro transitorio no guardado; un SVF pasa-banda extra |
| Agarrar un pico | Con el espectro congelado, los 5 picos más salientes llevan etiqueta de frecuencia y nota; arrastrar uno crea una campana con Q derivada del ancho del pico | Detección de picos por prominencia sobre el espectro suavizado a 1/6 de octava |
| Selección múltiple | Rectángulo, Ctrl+clic, Shift+clic; mover varias; escalar ganancias; copiar y pegar bandas entre instancias | Un comando `SetParams` por gesto (ya existe el patrón de edición sellada) |
| Entrada numérica | Doble clic en el nodo o en el valor; acepta "2k", "440", "A4", "C#2+13"; Tab recorre | Analizador de texto en `kit.rs`, reutilizable en todos los dispositivos |
| Pantalla | Rango ±3/6/12/30 dB con ampliación automática; zoom horizontal; teclado de piano; tamaño ampliado | Hoy 18 o 30 dB |
| Salida | Escala de ganancia global 0–200 %, inversión de polaridad, pan o balance M/S | Auto gain actual se mantiene |
| Dibujar la curva | Trazar la curva con el ratón y ajustar N bandas | Mismo ajuste que el EQ match: selección codiciosa de picos más mínimos cuadrados |
| Igualar a una referencia | Aprender el espectro propio y el de una pista de referencia o un archivo durante 10 a 30 s; proponer de 3 a 12 bandas como **propuesta** aceptable o descartable | Encaja con el sistema de propuestas de `aitrack-model` |
| Colisiones | Sombrear las zonas donde esta pista y otra elegida tienen energía a la vez | Función del host (§5.3) |

Nombres propios sugeridos para los modos: Directo, Analógico, Fase lineal. Para la dinámica: "Rango dinámico", "Solo picos" (la variante espectral).

### 3.2 Limitador de clase Pro-L

#### Lo que hace Pro-L 2 [P, ayuda de FabFilter]

**Estilos (8)**

| Estilo | Comportamiento descrito por FabFilter |
|---|---|
| Transparent | Fiel al original, evita bombeo y coloración |
| Punchy | El más audible; apretado, bombea un poco. Para pistas sueltas y música con ritmo marcado |
| Dynamic | Realza los transitorios antes de limitar para conservar pegada |
| Allround | Equilibrio entre loudness y transparencia |
| Aggressive | Cercano al recorte. Para EDM, rock, metal |
| Modern | Por defecto en la v2. Transparente, llega a mucho loudness y funciona con lookahead casi nulo |
| Bus | No transparente a propósito: pegar, bombear, aplastar buses de batería |
| Safe | Pensado para no distorsionar nunca; para material delicado |

Los cuatro últimos necesitan más latencia.

**Dos etapas.** Una etapa rápida de transitorios, gobernada por el Lookahead, y una etapa lenta de release, gobernada por Attack y Release, que responde a la dinámica media. Lookahead corto conserva transitorios y da más loudness aparente con riesgo de distorsión; por debajo de 0,1 ms se acerca al recorte duro. Ataque corto con release largo es más limpio y puede bombear; ataque largo con release corto da más presencia y puede distorsionar. Los rangos numéricos no están en la página [NV].

**Enlace de canales** separado para las dos etapas: Transients (suele ir por debajo del 100 %) y Release (se empieza en 100 %).

**Pico real.** Medición de pico real conmutable (cumple BS.1770 y R128) y limitación de pico real que garantiza el techo. Sobremuestreo hasta 32× [P, doc 04]. Recomendación del fabricante: 4× con al menos 0,1 ms de lookahead mantiene los picos entre muestras dentro de unos 0,1 dB.

**Techos recomendados** en la ayuda: −1,0 dBTP (EBU R128, Spotify, YouTube), −2,0 dBTP (ATSC A/85), −0,1 dBTP (CD, iTunes).

**Pantalla en tiempo real.** Entrada en gris azulado, salida en azul oscuro, reducción en rojo desde arriba con los picos de reducción etiquetados, y una línea fina de loudness. Modos: Slow Down, Fast, Slow, Infinite (todo el audio desde que se abrió) y Off. Disposición compacta que oculta la pantalla.

**Medidores.** Escalas de 16, 32 y 48 dB; K-12, K-14 y K-20; y escala de loudness. Pico y RMS a la vez (el RMS sigue la especificación Momentary de R128). El número de pico máximo cambia de color: verde con pico real activo, naranja si supera el techo, rojo si pasa de 0 dBTP.

**Loudness.** Momentary, Short Term e Integrated; el elegido se ve grande y los otros dos como barras. Máximo momentáneo o a corto plazo. LRA cuando se elige Integrated. Pausa, reinicio y reinicio automático al arrancar la reproducción. Objetivos: −9 LUFS (CD), −14 LUFS (streaming), −23 LUFS (EBU R128), −24 LUFS (ATSC A/85) y personalizado. Escala de +9 o +18 LU sobre el objetivo, absoluta o relativa.

**Opciones.** Unity Gain (el nivel de salida pasa a ser el inverso de la ganancia, para oír el efecto sin el aumento de volumen; los medidores se ponen amarillos). Audition Limiting (se oye solo la diferencia). Filtro de DC. Disparo por sidechain externo (para masterizar por stems). Lock Output al comparar presets. Bypass compensado.

**Dither.** 24, 22, 20, 18 o 16 bits. Noise shaping Basic (sube el ruido por encima de 6 kHz), Optimized (más bajo en conjunto, sube más por encima de 10 kHz) y Weighted (según la sensibilidad del oído, diseñado para 44,1 kHz).

#### Nuestra especificación (`fx.limiter`, evolución del actual)

Estado actual: ganancia 0–24 dB, techo −12–0 dB, release 1–1000 ms, release automático, lookahead 0,5–10 ms, pico real, sobremuestreo 1×/2×/4×, enlace estéreo. Núcleo: mínimo deslizante, media móvil y pico real 4×. Editor: envolvente desplazable de entrada y salida, techo arrastrable, reducción desde arriba, lecturas de pico real y LUFS S e I.

| Parámetro nuevo | Rango y valor inicial | Qué hace |
|---|---|---|
| Estilo | Transparente, Equilibrado, Pegada, Denso, Bus, Seguro; inicial Equilibrado | Preset de topología (tabla siguiente) |
| Ataque | 0–10 ms; 1 ms | Retardo antes de que actúe la etapa lenta |
| Enlace de transitorios | 0–100 %; 75 % | Enlace de canales de la etapa rápida |
| Enlace de release | 0–100 %; 100 % | Enlace de canales de la etapa lenta (el enlace actual pasa a ser este) |
| Sobremuestreo | 1×, 2×, 4×, 8× | Añadir 8× |
| Ganancia unitaria | Sí/No | Resta la ganancia de entrada a la salida para comparar a volumen igual |
| Escuchar diferencia | Sí/No | Salida = entrada retardada − salida |
| Filtro de DC | Sí/No | Pasa-altos de 5 Hz antes de detectar |
| Disparo | Interno, Sidechain | Para limitar stems con la suma como detector |
| Dither | No, 16, 20, 24 bits | TPDF |
| Forma del ruido | Plano, Suave, Psicoacústico | Filtro de error de orden bajo; el tercero según una curva de sensibilidad |
| Objetivo de loudness | Lista editable (§5.2) | Solo afecta a la medición y a los avisos |

Estilos como combinaciones del núcleo que ya tenemos (diseño propio; no pretende reproducir los de FabFilter):

| Estilo | Etapa rápida | Etapa lenta | Extra |
|---|---|---|---|
| Transparente | Lookahead 3–5 ms, ventana Hann | Release adaptativo largo (dos constantes) | — |
| Equilibrado | Lookahead 1,5 ms | Release adaptativo medio | — |
| Pegada | Lookahead 0,5–1 ms | Release corto, ataque de la etapa lenta 5–10 ms | Deja pasar 0,5–1 dB de transitorio a un recorte suave final |
| Denso | Lookahead 0,3 ms | Release muy corto | Pre-clipper suave con ADAA y sobremuestreo obligatorio 4× |
| Bus | Lookahead 1 ms, enlace 100 % | Release fijo audible, 80–200 ms | Rodilla ancha: empieza a actuar 3 dB antes del techo |
| Seguro | Lookahead 5–10 ms | Release largo, pico real siempre activo | Sin recorte final; la ganancia nunca sube más rápido que 20 dB/s |

Editor:
- Pantalla con velocidad elegible (rápida, lenta, todo el tema) y la curva de LUFS a corto plazo superpuesta.
- Etiquetas con el valor en los picos de reducción mayores de 1 dB.
- Techo arrastrable (ya existe) y ganancia arrastrable sobre la propia envolvente de entrada.
- Columna de loudness a la derecha: número grande (M, S o I a elegir), barras de los otros dos, LRA, pico real máximo, PLR (pico real menos LUFS integrado), y la marca del objetivo; el número cambia de color fuera de ±1 LU del objetivo.
- Color del pico real: normal por debajo del techo, aviso por encima del techo, error por encima de 0 dBTP. Los colores salen de `theme.rs`.
- Botones: ganancia unitaria, diferencia, reiniciar medición.

### 3.3 De-esser de clase Pro-DS

#### Lo que hace Pro-DS [P, ayuda y página de producto]

- **Threshold**: nivel de detección a partir del cual actúa. Un medidor circular alrededor del control enseña el nivel de la señal de detección ya filtrada. En modo Single Vocal el umbral puede bajar hasta −∞: toda la sibilancia se reduce más o menos lo mismo, lo que marque Range. Rango numérico no publicado en la página [NV].
- **Range**: escala y limita la reducción. Rango numérico no publicado [NV].
- **Filtros de detección HP y LP**: de 2 kHz a 20 kHz, con analizador en tiempo real sobre los deslizadores y botón de escucha de la señal de detección. Sibilancia vocal típica: 8 a 10 kHz.
- **Modo**: Single Vocal (algoritmo que distingue sibilancia de lo que no lo es; FabFilter no publica cómo) o Allround (solo banda y umbral; para mezclas enteras, platos, etc.).
- **Proceso**: Wide Band (baja toda la señal) o Split Band (solo baja los agudos; la frecuencia de corte sale del filtro HP de detección). La página de producto dice que el Split Band es de fase lineal; añade latencia.
- **Lookahead**: hasta 15 ms, con botón para desactivarlo; unos 10 ms suele ir bien en voces. Desactivado, en Wide Band y sin sobremuestreo, la latencia es cero.
- **Stereo Link**: 0 a 100 %; más allá del 100 %, solo Mid o solo Side.
- **Sidechain** interno o externo. **Escucha del disparo** (se oye solo lo que se está quitando). Sobremuestreo hasta 4×.
- **Pantalla**: forma de onda desplazable con las partes reducidas resaltadas, medidor de reducción y lecturas.

#### Nuestra especificación (`fx.deesser`, nuevo)

| Parámetro | Rango | Inicial | Notas |
|---|---|---|---|
| Modo de detección | Voz, General | Voz | Ver DSP |
| Umbral | −60…0 dB (General); −24…+12 dB relativos (Voz) | −30 dB / 0 dB | En Voz es relativo: no depende del nivel de la pista |
| Rango | 0…24 dB | 8 dB | Reducción máxima |
| Banda baja | 1…16 kHz | 5 kHz | Límite inferior de detección y, en banda partida, cruce |
| Banda alta | 2…20 kHz | 12 kHz | Límite superior de detección |
| Proceso | Banda partida, Banda ancha | Banda partida | |
| Release | 10…300 ms | 60 ms | Ataque fijo de 0,2 ms |
| Lookahead | 0…10 ms | 2 ms | Latencia declarada |
| Enlace estéreo | 0…100 % | 100 % | |
| Destino | Estéreo, Mid, Side | Estéreo | |
| Disparo | Interno, Sidechain | Interno | |
| Escucha | No, Detección, Diferencia | No | |
| Salida | −12…+12 dB | 0 dB | |

DSP propuesto:
- **Detección General**: la señal pasa por un pasa-banda (SVF HP de 12 dB/oct en "banda baja" y LP en "banda alta"); seguidor de pico rápido; ganancia = −min(rango, (nivel − umbral)·k) con rodilla suave (computador de Giannoulis et al. con ratio alta, por ejemplo 5:1).
- **Detección Voz**: se mide la energía de la banda de sibilancia y la energía de banda ancha con el mismo seguidor, y se usa la **diferencia en dB** como señal de control. Una "s" tiene casi toda su energía en la banda; una vocal brillante, no. Se añade un segundo indicio barato: la tasa de cruces por cero en ventanas de 5 ms, que sube mucho en fricativas. El umbral se aplica a esa medida relativa, así que el de-esser reduce igual una "s" suave y una fuerte. Es un diseño nuestro a partir de técnicas conocidas; no pretende ser el algoritmo de FabFilter, que no está publicado.
- **Banda partida sin crossover**: salida = x − (1 − g)·hp(x), donde hp es un pasa-altos (o pasa-banda) de fase mínima en la frecuencia de cruce. Con g = 1 la señal pasa intacta bit a bit; solo se resta parte de los agudos cuando hay reducción. Es el mismo principio de resta que usa el de-esser de futureboard-studio (MIT); se reescribe con nuestras primitivas (`Svf`, `EnvelopeFollower`, `GainSmoother`, `DelayLine`). Variante de fase lineal en v2 con un FIR corto (latencia de unas 64 muestras).
- **Banda ancha**: g multiplica toda la señal retardada por el lookahead.
- Latencia = lookahead. Medidor: reducción en dB, nivel de detección, umbral.

Editor:
- Arriba, forma de onda desplazable; los tramos donde actúa se pintan con el color de acento y la reducción cuelga desde arriba (misma gramática que el limitador y el compresor).
- Abajo, espectro de 1 a 20 kHz con la banda de detección como un rango arrastrable (bordes = banda baja y alta; arrastrar el centro mueve ambos). La línea de umbral se arrastra en vertical sobre el nivel de detección.
- Botones de escucha (detección, diferencia) y conmutador banda partida o ancha.

Presets: Voz femenina, Voz masculina, Voz rap brillante, Coros, Hi-hats y platos, Overheads, Sinte áspero (General, 3–8 kHz), Máster suave (banda ancha no; partida, rango 3 dB).

### 3.4 Sintetizador wavetable de clase Serum

#### Lo que hace Serum

**Serum 1** ([S] Sound On Sound y EDMProd salvo indicación; el manual de la v1 no se pudo abrir en esta sesión):
- Dos osciladores wavetable (A y B), un sub y un oscilador de ruido que es un reproductor de samples estéreo (con tono, fase, one-shot y seguimiento de teclado).
- Tablas de hasta 256 cuadros [S]; cada cuadro tiene 2048 muestras [M]. Visor 2D y 3D; WT POS recorre la tabla.
- Editor de tablas: importar audio, dibujar, fórmulas matemáticas, edición por armónicos con FFT [M], interpolar cuadros por crossfade o por morph espectral, exportar WAV.
- Menú Warp por oscilador. Modos citados: sync (varios), Bend +, Bend −, Bend +/−, PWM, Asym, Flip, Mirror, Remap 1–4, Quantize, FM, AM y RM desde el otro oscilador [S, lección de CreativeLive; lista completa M].
- Unísono de hasta 16 voces por oscilador [S, LANDR] con desafinación y mezcla; en la pestaña Global, modos de reparto Linear, Super, Exponential, Inverse y Random, y Stack (octavas y quintas). Se puede repartir la posición de tabla y el warp entre las voces.
- Sub: seno, rectángulo redondeado, triángulo, sierra, cuadrada y pulso; octava, nivel, pan y "Direct Out" (se salta filtro y efectos).
- Un filtro con unos 90 tipos en cuatro categorías: Normal (paso bajo, alto, banda, notch, pico), Multi (dos o tres filtros combinados; el control FAT pasa a ser el segundo corte), Flanges (peines, phasers, flangers) y Misc. Cutoff, resonancia, pan, drive, FAT, mezcla, seguimiento de teclado y casillas para elegir qué fuentes pasan por él.
- Tres envolventes con curvas ajustables por tramo y fase Hold. La 1 es el volumen.
- Cuatro LFO (hasta 8 al asignarlos) con forma dibujable sobre rejilla de resolución variable. Modos Trig, Env (un solo ciclo, como envolvente a medida) y Off (libre). Rate con BPM y Anch. Shift+arrastre dibuja escalones ajustados a la rejilla. Hasta 100 Hz en libre y hasta 32 compases en sincronía. Formas guardables.
- **Modulación por arrastre**: se arrastra la cabecera del modulador hasta un control. El rango aparece como un arco azul alrededor del control y se ajusta alargando o acortando el arco. La cabecera muestra cuántos destinos tiene.
- Matriz de 16 filas [S]: fuente, destino, cantidad, curva de respuesta, unipolar o bipolar, inversión y fuente auxiliar (por ejemplo, la rueda controla la profundidad del vibrato). Fuentes extra: velocidad, nota, aftertouch, aleatorios y dos fuentes de caos.
- Cuatro macros.
- Diez efectos reordenables: Hyper/Dimension, Distortion, Flanger, Phaser, Chorus, Delay, Compressor (con modo multibanda), Reverb, EQ y Filter. Casi todos sus parámetros se pueden modular.

**Lo que añadió Serum 2** [P, "What's New in Serum 2", manual 1.0.0 del 17-03-2025, y página de producto]:
- Tres osciladores principales (antes dos), cada uno con cinco modos: Wavetable, Multisample (SFZ), Sample (puntos de inicio, fin y bucle modulables, crossfade, troceado automático y manual), Granular (hasta 256 granos) y Spectral (resíntesis armónica con control independiente de tiempo y tono).
- Wavetable: dos warps a la vez ("Dual Warp"), modos nuevos (FM real, PD, y warps que vienen de filtros y distorsiones), interpolación suave de la posición, memoria y aleatoriedad de fase, modos de afinación (semitono, armónicos, ratio), copiar y pegar osciladores con o sin modulaciones, unísono ampliado.
- Dos filtros en serie o en paralelo, con tipos analógicos virtuales nuevos y un modo limpio del Drive.
- Mezclador nuevo: canales de osciladores, de filtros y de buses de FX, con enrutado y balance hacia filtros y buses.
- Cuatro envolventes con opción BPM y "legato invertido".
- Hasta 10 LFO (del 7 al 10 aparecen al asignar el 6), editor dedicado con herramientas de dibujo, rejilla X e Y independientes, presets, fase modulable, velocidades de hasta 1000 Hz, seguimiento del swing, modos de caos Lorenz y Rössler.
- Ocho macros, cada una también destino de modulación. Cualquier oscilador o filtro puede ser fuente.
- Matriz: quitar, saltar y reordenar modulaciones, vista ampliada, curvas editables de fuente y de fuente auxiliar.
- FX: 13 efectos y 3 módulos divisores (graves/agudos, graves/medios/agudos, Mid/Side), dos buses de FX además del principal, varias instancias del mismo efecto, presets de rack y de módulo. Nuevos: Bode (desplazador de frecuencia), Convolve, Utility; el Delay gana modo HQ, la Distortion un modo Overdrive y la Reverb tres tipos (Vintage, Nitrous, Basin).
- Secuenciador de clips con piano roll, 12 ranuras por banco, carriles de automatización y salida MIDI. Arpegiador con 12 ranuras y editor de patrones. Teclado con tonalidad, escala, swing y mapa de osciladores por nota y velocidad.
- Navegador de presets con etiquetas, valoraciones y escucha previa. Deshacer y rehacer completos. 626 presets y 288 tablas. Precio 249 USD; gratis para quien tenía Serum 1.

#### Nuestra especificación (`instrument.wavetable`, evolución por fases)

Estado actual: 2 osciladores con 6 tablas fijas (Saw, Square, Pulse, Sine-Saw, Triangle, Sine), posición, afinación, nivel, pan, unísono hasta 7 voces con desafinación y apertura; sub (seno o cuadrada, −1 o −2 octavas); ruido blanco; un filtro SVF (LP, HP, BP; 12 o 24 dB), drive, seguimiento de teclado y envolvente; ADSR de amplitud y de modulación; 2 LFO globales de 7 formas fijas; matriz de 8 ranuras (9 fuentes, 12 destinos); glide, poly/mono/legato; chorus, delay y reverb pequeños; 16 voces. Editor: tablas con arrastre de posición, filtro con nodo, ADSR arrastrables, formas de LFO y matriz en tabla. 68 presets.

**Fase 1: lo que más se nota (esfuerzo alto, valor muy alto)**

| Bloque | Especificación |
|---|---|
| Tablas | Formato: cuadros de 2048 muestras, hasta 256 cuadros, guardados como WAV mono (se acepta el troceado de 2048 muestras que usan las tablas comerciales). Banco de fábrica de 60 a 100 tablas propias generadas por código: analógicas, PWM, sync, formantes y vocales, armónicas, FM, espectrales, "sucias". Mipmaps por octava calculados fuera del hilo de audio (ya lo hace `crates/dsp/src/wavetable.rs`) |
| Importar | Arrastrar un WAV al oscilador. Modos: cortar cada 2048 muestras; detectar el periodo (autocorrelación o YIN) y remuestrear cada ciclo a 2048; un solo ciclo. La tabla viaja con el preset (ruta más copia en la carpeta del proyecto) |
| Interpolación | Entre cuadros: crossfade (actual) o morph espectral (interpolar magnitudes y fases desenrolladas, precalculado al cargar) |
| Warp | Dos ranuras por oscilador, cada una con modo y cantidad 0–100 %. Modos de fase (se aplican a la fase antes de leer la tabla): Sync (la fase corre más rápido y se reinicia, con ventana para suavizar), Curvar + / − / ± (potencia sobre la fase), Ancho de pulso (comprime el ciclo y rellena con silencio), Asimetría, Espejo, Invertir, Cuantizar (escalones), Remapear (curva dibujada). Modos entre osciladores: FM desde el otro oscilador, sub o ruido; AM; RM. Cada modo declara cuántos armónicos añade para elegir el nivel de mipmap y sobremuestrear 2× cuando haga falta |
| Unísono | Hasta 16 voces por oscilador. Desafinación, mezcla (centro frente a lados), apertura estéreo, reparto (lineal, tipo supersaw, exponencial, aleatorio), apilado (octava, quinta), dispersión de posición de tabla y de warp, fase inicial y aleatoriedad de fase. En SIMD de 8 con `wide` |
| Sub | Seno, triángulo, sierra, cuadrada, pulso, seno redondeado; −2 a 0 octavas; salida directa (se salta filtro y efectos) |
| Ruido | Reproductor de samples cortos (blanco, rosa, vinilo, aire, ataques) con tono, seguimiento de teclado, one-shot, fase aleatoria y salida directa |
| LFO dibujables | 4 LFO (hasta 8). Forma: lista de puntos (x, y) con curvatura por tramo, hasta 64 puntos. Rejilla X de 2 a 32 e Y de 2 a 16, con imán. Herramientas: punto, línea, escalón, rampa, y formas predefinidas. Modos: Disparo (reinicia en cada nota), Envolvente (un ciclo y se detiene), Libre (sigue al transporte). Velocidad en Hz (0,01–100) o en compases (1/64 a 32, con puntillo y tresillo), subida (fade-in), retardo, suavizado, fase. Por voz o global. Formas guardables y compartidas con `fx.shaper` (§4.3.2) |
| Arrastrar para modular | Cada fuente (LFO, envolvente, macro, velocidad, nota, rueda, aftertouch) tiene un asa. Se arrastra hasta cualquier control; aparece un arco de profundidad en el control, que se ajusta arrastrando el arco (Alt = bipolar, doble clic = quitar). El control muestra un punto animado con el valor modulado. La cabecera de la fuente lleva el número de destinos. Al pasar el ratón por una fuente se iluminan sus destinos, y al revés |
| Matriz | 32 filas: fuente, curva, fuente auxiliar (multiplica), cantidad, unipolar o bipolar, destino, saltar. Es la misma información que los arcos, en tabla |
| Macros | 8, con nombre, asignables por arrastre, automatizables desde el DAW y expuestas en la tarjeta del dispositivo |
| Envolventes | 3: retardo, ataque, hold, decay, sustain, release, con curvatura por tramo arrastrando el segmento |

**Fase 2: filtros y efectos**

| Bloque | Especificación |
|---|---|
| Filtros | Dos filtros, en serie o en paralelo, con envío por fuente (qué osciladores pasan por cuál). Tipos: SVF 12 y 24 dB (LP, HP, BP, notch, pico), ladder de 4 polos con no linealidad tanh, peine + y −, formante (vocales), phaser, combinados (LP+HP con dos cortes), ring-mod, sample-and-hold. Todos ZDF (Zavalishin), con drive y un segundo parámetro por tipo |
| Rack de efectos | Módulos reordenables por arrastre, cada uno con mezcla y bypass: distorsión (reutiliza los shapers de `fx.saturator`), multibanda ascendente y descendente (núcleo de `fx.multiband`), chorus, ensemble, flanger, phaser, delay, reverb, EQ de 3 bandas, filtro, compresor y utilidad. Los parámetros de los efectos son destinos de modulación |
| Voz | 32 voces; mono, legato, portamento con curva y "siempre"; afinación por nota (pan, tono, corte) en ciclos de hasta 8 notas |
| Calidad | Sobremuestreo 1× o 2× del núcleo de osciladores; el filtro ladder a 2× |

**Fase 3: lo que añadió Serum 2 y nos interesa**

- Tercer oscilador con modo Sample (reutiliza `instrument.sampler`) y Granular (hasta 64 granos en v1).
- Divisores de banda en el rack (graves/agudos, tres bandas, Mid/Side) con `Crossover3`.
- Moduladores de caos (Lorenz) y aleatorios suavizados.
- Editor de tablas: dibujar un cuadro, editar armónicos (magnitud y fase de los primeros 128), morph entre cuadros, normalizar, quitar DC; y "resamplear a tabla" desde un clip de audio del proyecto.
- El arpegiador y el secuenciador de clips **no** van dentro del sintetizador: en nuestro DAW eso lo hacen los clips MIDI y los futuros efectos MIDI de host, que sirven a todos los instrumentos.

Editor (diseño propio): una página de osciladores con el visor de tabla en perspectiva y el cuadro activo resaltado; el warp se ve deformando el cuadro en vivo. Debajo, una franja de moduladores siempre visible (LFO y envolventes en pestañas, con sus asas de arrastre). Páginas: Osciladores, Efectos, Matriz. Los colores de cada fuente de modulación salen del tema y se repiten en los arcos de los controles.

Advertencia de licencia: Vital y Surge son GPL y no se copian. Las tablas de fábrica de Serum, Vital o Massive no se redistribuyen; las nuestras se generan por código. El formato de cuadros de 2048 muestras en WAV es un uso extendido y no una marca.

<!--CONT-->
