# 14 · Calidad de audio al importar un sample a una pista de clips

Motivo: el sonido de un archivo cambia al importarlo (arrastrar un sample del navegador al arrangement, o *File → Import audio*). Un clip recién importado, sin warp ni transposición y a 0 dB, debería sonar idéntico al archivo (bit a bit cuando la frecuencia de muestreo coincide).

Metodología: se leyó el camino completo (decodificación → caché/SRC → compilación del clip → `render_audio` → fader/pan → salida/export) y se montó una sonda desechable que enlaza el código real del motor (`aitrack-engine` por ruta, sin tocar el repo) y renderiza con el mismo grafo que la reproducción (`render_offline_with`). Todas las cifras de "VERIFICADO" salen de esa sonda. Sonda y WAVs de prueba: `/private/tmp/claude-501/-Users-corx--claude-spares-spare-MA5Yx6/2f35022c-3710-4989-a632-a34b1be5a151/scratchpad/audio-quality/` (`probe/src/main.rs`; se ejecuta con `target/release/probe <null|interp|srcnull|align|joins|frac|tempo|preview|pan|hf|warpmodes|wavgain>`).

Convención: **VERIFICADO** = reproducido con la sonda (o leído sin ambigüedad en código y comprobado con aritmética). **SUPUESTO** = deducido del código, no medido.

---

## 0. Resumen ejecutivo (por impacto audible)

| # | Defecto | Severidad | Estado |
|---|---|---|---|
| D1 | El SRC de `resample()` descuenta dos veces el retardo del filtro: **se recortan los primeros 2,7–2,9 ms** de todo archivo cuya frecuencia ≠ la del dispositivo (ataque del bombo/caja) y el audio queda adelantado | Crítica | VERIFICADO |
| D2 | La previsualización del navegador suena **6 dB más baja** (`GAIN = 0.5`) y por otro camino de SRC/interpolación que el clip | Alta | VERIFICADO |
| D3 | Los loops de librería se **auto-warpean** (modo Beats) a un BPM que viene del nombre o, si no hay, **se estima solo por duración**; Beats degrada material sostenido | Alta | VERIFICADO (código + SNR) |
| D4 | Rampa fija de 64 muestras a cero en el final de **todo clip sin fade-out**: muesca de 1,3 ms en cada unión de clips contiguos de un loop continuo | Alta | VERIFICADO |
| D5 | Un clip con warp ON a tempo igual al original sale del camino bit-exacto y pasa por un pasabajos de 16 taps (−1,5 dB a 20 kHz; hasta −12 dB a 23 kHz según la fase) | Media | VERIFICADO |
| D6 | Render offline (export, bounce, freeze, consolidar): los **primeros 32 muestras tienen fade desde 0** (fader y Main parten con ganancia 0) | Media | VERIFICADO |
| D7 | Un clip sin warp tiene longitud fija en ticks: subir el tempo después de importar **corta la cola** del sample; los no-WAV entran con longitud 4 compases fija | Media | VERIFICADO (código + aritmética) |
| D8 | Con inicio fraccionario en muestras (cualquier tempo que no sea 120/…) se **pierde el frame 0** del archivo | Baja | VERIFICADO |
| D9 | Otros: escala 32767 vs 32768, `pan_gains(0)` = 0,99999994, MP3 sin recorte gapless, sin AAC/M4A en el motor, clave de caché de warp a 0,001, export con doble SRC, alias en preview de 88,2/96 kHz | Baja | mixto |

Lo que **no** es causa (verificado): decodificación PCM 16/24/32f (exacta), mono→estéreo (copia a 0 dB, sin pérdida), sin normalización ni ganancia al importar, sin recodificación (el proyecto referencia la ruta del archivo), sin limitador/soft-clip implícito en el motor, sin dither en la salida en vivo, y la calidad del SRC rubato como filtro (plana hasta 20 kHz; el problema es su alineación, D1).

---

## 1. Importación y decodificación

Camino real: `Workspace::import_audio` (`crates/app/src/workspace.rs:1841`) o `add_sample_clip` (`:1878`, soltar un sample del navegador sobre un carril, `crates/app/src/views/arrangement.rs:512`) → `audio_track_commands` (`workspace.rs:2505`) crea `AudioSource` (ruta al archivo, **sin copiarlo ni recodificarlo**, `hash: None`) + `Clip::new` (0 dB, sin fades, offset 0) + `PlaceClip`. El archivo se decodifica al compilar el grafo: `SampleCache::get` → `load_audio` (`crates/engine/src/sample_cache.rs:300`).

Hallazgos (todos VERIFICADO salvo nota):

- Decoder: WAV con `hound` (`sample_cache.rs:146`), el resto con `symphonia` (`:175`). Enteros a `f32` con `1/2^(bits-1)` (`:153`): 16 y 24 bits son exactos en `f32`. Prueba D del nulo: 16 y 24 bits dan residuo ≤ −135 dB en el interior (solo difiere el 1 ulp de D9-b).
- Canales: mono → ambos canales a 0 dB (`clip.rs:779`, `sr = sl`); no hay atenuación ni "ley de pan" al importar. Prueba C: idéntica a estéreo. Más de 2 canales: solo se leen el 0 y el 1 (`clip.rs:719`, `:829`); el resto se descarta (SUPUESTO sobre el impacto, es raro).
- Sin normalización ni ganancia: `Clip::new` fija `gain_db: 0.0` (`project.rs:399`); la pista nace a 0 dB y pan 0 (`project.rs:298-299`). La normalización solo existe en la extracción de baterías de IA (`workspace.rs:1786`, `normalize_db: Some(-1.0)`) y en export si se activa (`engine.rs:711`, −0,3 dBFS).
- Picos/forma de onda: `waveform.rs:256` usa `decode_file` a la frecuencia nativa, solo para pintar. No afecta al audio.
- Longitud del clip: `audio_track_commands` (`workspace.rs:2518-2521`) la fija en ticks a partir de la duración y del tempo del proyecto, redondeada al tick (en 120 BPM, 25 muestras; en 126 BPM, ≈23,8) y mínimo 1 negra. Para no-WAV no hay cabecera hound: `import_audio` cae en `(48_000, 2, 0)` → `4 * BAR` (ver D7).

### D9-d · MP3 sin recorte gapless y formatos que el motor no abre — VERIFICADO en código, impacto SUPUESTO
`decode_symphonia` usa `FormatOptions::default()` (`sample_cache.rs:191`); en symphonia 0.5.5 `enable_gapless` es `false` por defecto (`symphonia-core-0.5.5/src/formats.rs:98`). Los MP3 conservan el retardo de codificador (típicamente ~1105 muestras ≈ 25 ms) y el relleno final: el golpe queda ~25 ms tarde respecto a la cuadrícula. Además `crates/engine/Cargo.toml:32` solo habilita `wav, pcm, flac, mp3, ogg, vorbis, aiff` (el crate `ai` sí añade `aac, isomp4, alac`): un M4A se puede analizar pero no reproducir.
**Corrección:** `FormatOptions { enable_gapless: true, ..Default::default() }` en `sample_cache.rs:191` y alinear las features de symphonia con las de `crates/ai/Cargo.toml:24`.

---

## 2. Conversión de frecuencia de muestreo (archivo ≠ dispositivo)

Dónde: **offline al cargar**, no en tiempo real (`load_audio` → `resample`, `sample_cache.rs:248`): rubato `SincFixedIn`, `sinc_len 256`, 256× de sobremuestreo, interpolación cúbica, ventana Blackman-Harris², `f_cutoff = calculate_cutoff`. Clave de caché `(ruta, frecuencia)` (`:317`). Con `from == to` devuelve una copia (`:250`), bit-exacta.

### Calidad del filtro — correcta (VERIFICADO)
Ganancia de tonos puros tras `resample` (misma función):

| Conversión | 15 kHz | 19 kHz | 20 kHz | 20,5 kHz | 21 kHz | rechazo |
|---|---|---|---|---|---|---|
| 44,1 → 48 kHz | 0,000 dB | 0,000 | −0,002 | −0,66 | −9,6 | 21,5 kHz: −41 dB |
| 48 → 44,1 kHz | 0,000 | 0,000 | −0,006 | — | −9,3 | 22 kHz: −100 dB |
| 96 → 48 kHz | — | — | 0,000 | — | — | 22k: −0,9; 23k: −9,9; 24,5k: −64 dB; ≥26k: −138 dB |

Es de calidad "Hi-Q": plano hasta ~20 kHz, sin aliasing apreciable. **No es la causa de que cambie el timbre; la causa es la alineación (D1).**

### D1 · El retardo del filtro se descuenta dos veces — CRÍTICA, VERIFICADO
`resample` (`sample_cache.rs:268`) toma `delay = rs.output_delay()` y al final hace `o.drain(..delay)` (`:293`), con el comentario "the filter delay is removed, so the result is time-aligned" (`:246`). Pero el `SincFixedIn` de rubato 0.16.2 **ya entrega la salida alineada** (empieza a leer media ventana antes del dato: `last_index = -(len/2)`, `asynchro_sinc.rs:312`); `output_delay()` es solo un número informativo. El crate hermano `ai` lo sabe y lo documenta (`crates/ai/src/audio.rs:221-224`: "its `output_delay()` must not be compensated a second time", `resample_sinc` pasa `delay = 0`); el motor no.

Evidencia:
- Impulso en el frame 1000 de un archivo de 44,1 kHz → pico esperado en 1088,44. Sin drenar, rubato lo deja en **1089**; con el `drain` del motor queda en **950** (138 muestras = **2,9 ms** antes). 48→44,1: pico en 802 en vez de 918,75. 96→48: 435 en vez de 500. 48→96: 1745 en vez de 2000.
- Sample tipo bombo (clic en el frame 0 + senoide decreciente) contra una referencia sinc independiente: residuo del camino real **+3,2 dB** (44,1→48), +2,8 dB (48→44,1) — es decir, no correlaciona —; **sin el drain: −46,8 dB** y −46,6 dB. En 96↔48 los residuos son −1,6 dB (real) frente a −32 dB (sin drain; limitado por mi referencia).
- Pico de los primeros 400 frames: −2,1 dB (44,1→48), −1,2 dB (48→44,1), −1,2 dB (48→96): el ataque original **desaparece** (los primeros 139 / 117 / 256 / 64 frames de salida se tiran).

Impacto audible: todo sample cuya frecuencia ≠ la del dispositivo (44,1 kHz de Splice sobre un dispositivo a 48 kHz, o al revés; archivos a 96 kHz) pierde su transitorio de ataque (2,7–2,9 ms: justo el "clic" del bombo, la caja, el hi-hat) y se adelanta ese tiempo. Capas de samples de distinta frecuencia quedan desfasadas ~3 ms entre sí (cancelaciones de fase). La previsualización **no** pasa por `resample` (ver D2), por eso suena "con pegada" y el clip importado, no. También afecta al export a otra frecuencia: `export.rs:240-243` usa `resample_buffer` → el mix exportado pierde sus primeras ~117 muestras y queda adelantado.
Afecta únicamente a archivos con frecuencia ≠ dispositivo; si coinciden, `resample` devuelve la copia y no ocurre.

**Corrección:** en `resample` (`sample_cache.rs:248-297`) eliminar la compensación: `let delay = 0;` (o quitar `output_delay()` y el `drain`), con la condición del bucle de vaciado `while out[0].len() < expected`. Sin asignaciones nuevas en el hilo de audio (es carga en el hilo del cargador). Añadir test junto al código: impulso en 1000 → pico en `round(1000·to/from)` ± 1 para (44100,48000), (48000,44100), (96000,48000), (48000,96000), igual al de `crates/ai/src/audio.rs:340`. No hay datos persistidos que migrar (la caché es solo memoria).

### D9-e · Export con doble SRC — VERIFICADO en código
`render()` renderiza a la frecuencia de sesión (`opts.session_rate`, la del dispositivo) y remuestrea al final (`export.rs:236-243`). Un archivo a 44,1 kHz con dispositivo a 48 kHz y export a 44,1 kHz atraviesa dos SRC (44,1→48→44,1) más el D1 dos veces. **Corrección:** `real/engine.rs:697-711`: usar `session_rate = options.sample_rate` cuando el motor offline puede correr a esa frecuencia (el render offline no depende del dispositivo), y dejar el SRC final solo si no.

---

## 3. Estado por defecto del clip al importar

- `Warp::default()` = `enabled: false, mode: Beats, original_bpm: None, transpose 0, detune 0` (`crates/model/src/project.rs:620-624`). **Importar un archivo suelto no activa warp ni estira.** VERIFICADO: la prueba A (120 BPM, sin warp) deja el interior a 1 ulp del original (D9-b).
- `import_audio` lanza luego el análisis (clave/BPM) y `apply_analysis` (`workspace.rs:1951-1971`) **guarda `original_bpm` pero no activa el warp**; sin efecto sobre el sonido hasta que el usuario lo enciende. Al encenderlo (`views/bottom.rs:539-552`) usa ese BPM (potencialmente mal detectado) con modo Beats.
- Sin fades automáticos al importar (`fade_in/out` en 0), pero el motor aplica sus propias rampas de borde (D4).
- **Excepción: samples de librería marcados loop** (`add_sample_clip`, `workspace.rs:1907-1915`): `warp.original_bpm = item.bpm; warp.enabled = item.bpm.is_some()` y modo Beats. Es el caso "arrastrar un loop del navegador".

### D3 · Auto-warp de loops con BPM poco fiable y modo Beats — ALTA
- De dónde sale `item.bpm`: del nombre del archivo, de una detección de audio con confianza ≥ 0,4 (`crates/library/src/reanalyse.rs:59`, `crates/app/src/real/library.rs:80`) o, si no hay nada, de `estimate_loop_bpm` (`crates/library/src/analyze.rs:274`): "asume 4/4 y 1/2/4/8/16 compases, el tempo más cercano a 120 en 70..180" **solo a partir de la duración** (`analyze.rs:141-146`, fuente `Estimate`). Esa estimación se trata igual que un tempo real al arrastrar el loop.
- `LibraryItem` (`bridge.rs:1002`) no lleva ni la fuente ni la confianza del BPM, así que la UI no puede distinguirlas.
- Cuando `original_bpm` ≠ tempo del proyecto, `AudioClipData::of_bpm` (`clip.rs:336-370`) prerrenderiza fuera del hilo de audio con `cached_warp` (`clip.rs:278`) y el algoritmo de Beats (`dsp/src/warp.rs:659`). Medido con un seno puro de 440 Hz, SNR contra el seno ajustado (VERIFICADO, `warpmodes`):

| Proyecto / original | Beats | Tones | Complex | ComplexPro |
|---|---|---|---|---|
| 126 / 120 (×0,95) | 143 dB (solo recorta, no estira) | 20,6 dB | 76,5 dB | 76,5 dB |
| 120 / 128 | 143 dB | 17,3 dB | 77,4 dB | 77,4 dB |
| 100 / 128 (×1,28) | **4,2 dB**, amplitud −2 dB | 3,4 dB | 77,7 dB | 77,7 dB |

  Beats alarga repitiendo la cola de cada rebanada con decaimiento (`warp.rs:193-205`): bien para batería, mal para bajos, pads o mezclas. Texture es estocástico por diseño (no medible así); Re-Pitch cambia el tono.
- Comparación con Live: *Preferences → Record/Warp/Launch* ofrece "Auto-Warp Long Samples" (activable/desactivable), "Default Warp Mode" (Beats por defecto) y "Create Fades on Clip Edges" ([guía](https://pcaudiolabs.com/ableton-live-warping-part-1-setup/), [Sound on Sound](https://www.soundonsound.com/techniques/warping-101-ableton-live)); muchos usuarios lo apagan porque el auto-warp se equivoca ([Mixed In Key](https://mixedinkey.com/satellite/how-to-disable-auto-warp-in-ableton-live/)). AI-Track no tiene preferencia ni distingue la confianza.

**Corrección (diseño):**
1. `LibraryItem` gana `bpm_trust: Trust { Name, Audio(conf), Estimate }` (`bridge.rs:1002`, `real/library.rs:item`). En `add_sample_clip` activar warp solo si `Name` o `Audio` con confianza ≥ 0,6; con `Estimate` dejar `original_bpm = Some(..)` pero `enabled = false` (el usuario lo ve en el panel y decide).
2. Preferencia `Auto-warp`: `Off | Loops de confianza | Todo` (por defecto el segundo) en `plugin_prefs.rs`/prefs, y `Modo de warp por defecto` elegido por categoría de la librería: Beats para batería/percusión, Tones para bajo/voz monofónica, Complex para el resto (nunca Beats para material sostenido).
3. Si `|original_bpm − tempo| < 0,01 %` tratar como sin warp (D5).

### D7 · La longitud del clip no sigue al tempo / no-WAV a 4 compases — MEDIA
- VERIFICADO (aritmética del motor + código): `Command::SetTempo` solo cambia `p.tempo` (`command.rs:211-215`); la longitud de un clip sin warp está en ticks fijos. Un archivo de 4 s importado a 120 BPM (8 negras) dura 3,429 s a 140 BPM → **se pierden 571 ms de cola** (1241 ms a 174 BPM) con la rampa de D4; a 90 BPM sobran 1333 ms de silencio. Un bombo/808 largo o un loop se corta al subir el tempo. Live conserva el sample en tiempo real.
- VERIFICADO (código): para archivos que no son WAV, `hound::WavReader::open` falla y `import_audio` usa `(48_000, 2, 0)` → `length = 4 * BAR` (`workspace.rs:1846-1852`, `:2518-2521`). `apply_analysis` actualiza `frames` del source pero no el `Clip` (`workspace.rs:1951-1971`): un FLAC/MP3 de 3 minutos entra recortado a 4 compases, y uno corto con silencio detrás.
- `add_sample_clip` para no-WAV supone `48 kHz`/2 canales en `AudioSource` (`workspace.rs:1884-1885`); la duración es correcta pero los metadatos no.

**Corrección:** (a) leer longitud y frecuencia de cualquier formato con una sonda barata de symphonia (`codec_params.n_frames`/`sample_rate`) en `import_audio`, en lugar de `hound` + valores por defecto; (b) en `apply_analysis`, si el clip aún tiene la longitud provisional, emitir `Command::UpdateClip` con la longitud real; (c) `ops::set_tempo_keeping_samples(p, bpm)` que devuelva un `Batch` con `SetTempo` + `UpdateClip` de cada clip de audio sin warp (longitud, fades, envolventes y bucle escalados con `bpm_old/bpm_new`, como ya hace `ops::stretched` en `ops.rs:200-221`); la UI y los agentes lo usan en vez de `SetTempo` crudo.

---

## 4. Reproducción en el motor

### Camino "exacto" (sin warp, sin transposición) — correcto (VERIFICADO)
`AudioClipData::exact()` (`clip.rs:374`) lee tramas enteras con `DecodedAudio::frame` (`sample_cache.rs:40`); `render_audio` (`clip.rs:752-806`) no interpola. Prueba A2 (interior, 48 kHz estéreo `f32`): `maxdiff = 1,19e-7` (−138,5 dBFS), residuo/señal = −135 dB, ratio RMS 0,0000 dB. Con 16/24 bits, mono y offset≠0: igual. Es decir, **el camino principal es casi transparente**; los defectos están en los bordes, en el SRC y en las excepciones siguientes.

### D4 · Rampa de 64 muestras a cero al final de todo clip sin fade-out — ALTA
`EDGE_RAMP = 64` (`clip.rs:498`); `ramp_out = shape.fade_out.length == 0` (`:723`) se aplica **siempre**, también cuando el clip termina exactamente donde termina el archivo o cuando el siguiente clip empieza justo detrás (`ramp_in` solo se aplica con `offset != 0`, `:722`). Medido (VERIFICADO, `joins`): tres copias contiguas de un loop sinusoidal continuo de 1 s: en la unión el audio cae a 0 durante ~64 muestras (en n=−6: −0,018 en vez de −0,191), RMS de las últimas 64 muestras 6,7 dB por debajo de las siguientes; desviación máxima de la senoide continua 0,33 sobre pico 0,5. Es un "tic" periódico en cada repetición de un loop sostenido (bajo, pad). En la prueba A el último 64 de un clip aislado también queda atenuado (ratio 1/64 en la última muestra).
**Corrección** (`clip.rs:722-739`, `CompiledClip::audio`): calcular al compilar `cuts_tail`: el final del clip corta material (`looping.is_some()` o `offset + length·rate < longitud del archivo en ticks − ε`). `ramp_out = fade_out.length == 0 && cuts_tail`. Opcionalmente, si el clip siguiente en el arrangement empieza en `start + length` y es de la misma fuente contigua (`next.offset == this.offset + length`), no hay rampa (la unión ya es continua). Sin asignaciones: solo un `bool` más en `CompiledClip`.

### D5 · Warp ON a tempo igual al original sale del camino exacto — MEDIA
`exact()` exige `!warped` (`clip.rs:374-376`). Un clip con warp ON y `original_bpm == tempo` (el caso del loop de librería cuyo BPM coincide) tiene `time_scale = 1` y `cached_warp` devuelve el mismo `Arc` (`clip.rs:281-283`) pero se lee por `render_audio`'s ruta interpolada (`clip.rs:859-860`) con `DecodedAudio::sample`: núcleo de 16 taps Kaiser β=9 con corte 0,94 (`sample_cache.rs:86-134`), cuya fila de fase 0 **no es la identidad** (el seno cardinal vale ≠ 0 en enteros porque `CUTOFF ≠ 1`). Medido (VERIFICADO):
- Respuesta del interpolador en posición fraccionaria 0 / 0,25 / 0,5: 15 kHz −0,012 / −0,053 / −0,18 dB; 18 kHz −0,45 / −0,63 / −1,16; 20 kHz −1,55 / −1,84 / −2,35; 22 kHz −3,2 / −4,6 / −6,8; 23 kHz −3,9 / −6,3 / −12,4. La pérdida depende de la fase fraccionaria (modulación de los agudos).
- Nulo, interior, warp ON con `original = tempo`: ruido blanco: residuo −17,8 dB y −0,48 dB de RMS; ruido rosa: −35,1 dB y −0,012 dB; mezcla de prueba: −30 dB. Con warp OFF las mismas señales: −135 dB.
Audible: pérdida de "aire" (>16 kHz) en hi-hats/platillos de los loops auto-warpeados; con 44,1 kHz de dispositivo el recorte empieza antes en términos relativos.
**Corrección:**
1. En `AudioClipData::of_bpm` añadir `steady: bool` (el compilador ya conoce `project.tempo.points().len() == 1` y los carriles de tempo, como hace `export.rs:437`). Definir `unity = warped && steady && ratio == 1.0 && (original_bpm - project_bpm).abs() < 1e-4 * project_bpm` (pitch-preserving con `time_scale ≈ 1` cuenta igual). `exact()` pasa a `ratio == 1.0 && (!warped || unity)` y el camino de tramas enteras usa `spt` del tempo de inicio, que es correcto en tempo constante.
2. Mejorar `DecodedAudio::sample` (afecta también a transposición/Re-Pitch/preview): (a) 32 taps y tabla con `CUTOFF = 1,0` cuando el paso ≤ 1 (la fila de fase 0 pasa a ser exactamente δ); (b) para paso > 1 (transposición hacia arriba, preview de 96 kHz) usar corte `0,94/paso` con tablas por octavas, o prerrenderizar con rubato como ya se hace con los modos que conservan el tono. La tabla se construye fuera del hilo RT (`init_interpolator`), sin asignaciones en `process`.

### D8 · Se pierde el frame 0 con inicio fraccionario — BAJA, VERIFICADO
El camino exacto alinea con `start_exact.round()` (`clip.rs:753`) pero el bucle del arrangement entrega el rango desde `ctx.sample_of(p.start)` = techo (`graph.rs:1141`, `:1150`). Si la parte fraccionaria es < 0,5, el primer frame del archivo cae en una muestra fuera del rango y no suena (a 126 BPM, 3 de cada 6 posiciones de negra: `frame 0` no aparece; en `o[round]` hay 0 y el frame 1 está en `ceil`). Un clic inicial o un archivo recortado en caliente pierde su primera muestra.
**Corrección:** `clip.rs:753`: `let start_i = ctx.sample_of(start);` (mismo redondeo que el llamador) en el camino exacto, y análogo para `first_frame`.

### D6 · Primer bloque del grafo recién compilado: fade desde 0 — MEDIA, VERIFICADO
`TrackNode::cur_gain` nace en `[0.0; 2]` (`compile.rs:633`); la primera vez que corre el fader hace una rampa lineal 0→objetivo en 32 muestras (`graph.rs:1049-1066`) y lo mismo en Main: ganancia total del primer muestra = (1/32)² ≈ 0,001. En la reproducción en vivo `adopt` conserva `cur_gain` (`graph.rs:678`) y solo se nota en pistas nuevas; **en el render offline (export, bounce, freeze, consolidar, stems) el grafo es siempre nuevo**, así que un bombo en el compás 1 (muestra 0) sale con ratio 0,00098 en la muestra 0, 0,28 en la 16 y 1,0 en la 31 (prueba A). Las sends tienen el mismo patrón (`s.cur`).
**Corrección:** `TrackNode` gana `gain_primed: bool` (y `SendNode` igual); en `graph.rs:1054` si no está inicializado, `t.cur_gain = [tl, tr]` antes de calcular los pasos. Sin asignaciones.

### Otros puntos de la cadena (todos por código salvo indicación)
- **D9-b · `pan_gains(0)`** (`automation.rs:51-54`) da `0,99999994` en ambos canales (VERIFICADO, bits `0x3f7fffff`): −5e-7 dB, inaudible, pero rompe la transparencia bit a bit. Corrección: `if pan == 0.0 { return (1.0, 1.0) }`. La ley deja el opuesto sin mezclar y sube +3 dB el lado del pan (hard left = √2): es un balance con refuerzo, no la ley de Live; decisión de diseño, no un defecto de importación.
- **Ganancia master / limitador:** no hay soft-clip ni limitador en el motor (búsqueda de `tanh`/`clamp` en `engine/src`: nada). El proyecto vacío solo tiene Main; las plantillas "Electronic starter" y "Demo" añaden un Limiter (techo −1 dB, lookahead) en Main (`demo.rs:194`, `:266`) y, la demo, EQ "Master Polish": si el usuario compara contra el archivo en una plantilla, ese limitador sí cambia el sonido. SUPUESTO sobre cuánta reducción aplica.
- **Salida en vivo:** `T::from_sample` sin dither (`audio_io.rs:504`); si el dispositivo abre en `I16`, hay truncado sin dither (SUPUESTO: en macOS el dispositivo suele ser `F32`).
- **D9-c · `cached_warp`** redondea `time_scale` a milésimas en la clave (`clip.rs:280`): dos tempos a <0,1 % comparten entrada y el desvío acumulado llega a ~15 ms en 30 s. Corrección: cuantizar a 1e-6.
- **Tempo variable:** con carriles de tempo, los clips con warp pasan a lectores aproximados en tiempo real (`clip.rs:825-872`, `sample_warped_beats/grain`) de menor calidad que el prerrender; esperado, pero el cambio de timbre al activar automatización de tempo no está avisado en la UI.

---

## 5. Previsualización del navegador vs. clip

### D2 · −6 dB y otro camino — ALTA, VERIFICADO
`crates/app/src/real/preview.rs`: `GAIN: f32 = 0.5` (`:23`, aplicado en `:287-289`), comentado como "−6 dB: close to how the sample will sound in a track". El clip arrastrado suena a 0 dB: **el sample suena 6 dB más fuerte al importarlo** que en la audición. Por sí solo es suficiente para "suena diferente" (más volumen = más pegada percibida).
Además la previsualización decodifica a la frecuencia nativa y lee con `DecodedAudio::sample` a `paso = archivo/stream` (`:42`, `:56`) en vez de pasar por `load_audio`/rubato:
- Archivo de 96 kHz sobre stream de 48 kHz: un tono de 30 kHz aparece **a 18 kHz con 0,0 dB** (alias sin filtro; el clip, con rubato, lo rechaza a −138 dB). 88,2 kHz: −0,1 dB.
- 44,1 → 48 kHz: 19 kHz −2,6 dB y 20 kHz −4,3 dB (el clip, con rubato: 0,000 y −0,002 dB). 48 → 44,1: 20 kHz −1,8 dB.
- Cuando `paso == 1` se lee exacto (`:56`).
- Combinado con D1: el preview conserva el ataque y el clip importado no.

**Corrección:** (a) `GAIN = 1.0` y, si se quiere margen, un control de volumen de audición explícito en preferencias (como el "Cue Volume" de Live); (b) en `Worker::play` (`preview.rs`), para archivos ≤ ~30 s (el límite `MAX_SECONDS` ya existente) decodificar con `sample_cache::load_audio(path, rate)` (carga en el hilo `aitrack-preview`, no en el callback) para que el preview use exactamente el mismo SRC y alineación que el clip; mantener el lector con `sample()` solo para transposiciones, ya con el filtro de D5-2.

---

## 6. Export / bounce

`crates/engine/src/export.rs`:
- Defecto de la UI: WAV 24 bits, 48 kHz, sin normalizar (`views/export.rs:54-57`): bien.
- Float32: exacto. Int24 (`:497`): escala `8_388_607` frente a decodificación `/2^23` → diferencia de 1,2e-7 (−138 dB), irrelevante.
- **D9-a · Int16** (`quantize16`, `export.rs:462`): escala `32_767` frente a la decodificación `/32_768`: un archivo de 16 bits importado y exportado sin dither no sale idéntico (VERIFICADO: 20000→19999, 32767→32766, −32768→−32767; −0,00027 dB). Con el dither TPDF por defecto de `Int16Dithered` (`real/engine.rs:217`) añade además ruido de ±1 LSB (es lo habitual para bajar a 16 bits; correcto cuando se reduce profundidad, innecesario si el contenido ya era de 16 bits). **Corrección:** `x * 32768.0` redondeado y recortado a ±32767 (y 24 bits `* 8_388_608.0` con recorte).
- D6: primeras 32 muestras con fade (también para stems y consolidación).
- D1 y D9-e: el SRC final pierde ~117 muestras iniciales y hay doble conversión si archivo y export coinciden pero el dispositivo difiere.
- Normalización opcional a −0,3 dBFS (`engine.rs:711`): una sola ganancia, bien.

---

## 7. Comparación con Ableton Live

| Aspecto | Live | AI-Track hoy |
|---|---|---|
| Warp al importar | Preferencia "Auto-Warp Long Samples" y "Loop/Warp Short Samples" (Auto/On/Off), modo por defecto configurable (Beats) | Loops de librería: auto-warp con BPM del nombre o **estimado por duración**, modo Beats fijo; archivo suelto: sin warp |
| Longitud en tempo variable | El sample sigue en tiempo real al cambiar el tempo | Longitud fija en ticks: se corta/silencia (D7) |
| Conversión de frecuencia | Interna, de alta calidad ("Hi-Q" para Re-Pitch/transposición) | Filtro rubato bueno, mal alineado (D1); transposición con núcleo de 16 taps sin antialias (D5) |
| Fades de borde | "Create Fades on Clip Edges" (activable) | Rampa fija a cero al final de todo clip (D4) |
| Previsualización | Volumen de audición propio, mismo motor | −6 dB fijo, otro camino de SRC (D2) |
| Modos de warp | Beats, Tones, Texture, Complex, Complex Pro, Re-Pitch | Los seis; Complex/ComplexPro excelentes en la prueba (77 dB); Tones 17–21 dB; Beats degrada por encima de ~×1,2 en material sostenido |

(Nombres de preferencias de Live según guías de terceros; no se consultó el manual oficial: SUPUESTO en el detalle de valores por defecto.)

---

## 8. Orden de corrección recomendado

1. **D1** (`sample_cache.rs:248-297`): quitar el doble descuento de retardo + test de impulso. Una línea, efecto inmediato en todo archivo con frecuencia ≠ dispositivo y en el export a otra frecuencia.
2. **D2** (`preview.rs:23`): `GAIN = 1.0` y preview vía `load_audio`.
3. **D4** (`clip.rs:722-739`): rampa de cola solo cuando el clip corta material.
4. **D3 + D5**: política de auto-warp con confianza (`add_sample_clip`), y camino exacto para warp ≡ tempo (`of_bpm`/`exact`).
5. **D6** (`graph.rs:1049-1066`, `compile.rs:633`): primar `cur_gain`.
6. **D7**: longitud real en no-WAV; `set_tempo_keeping_samples`.
7. **D8**, **D9** (escala 32768, `pan_gains(0)`, gapless, `session_rate` de export, clave de caché).

Verificación prevista tras cada corrección (sin tocar `cargo fmt`): `cargo test -p aitrack-engine` (los tests de `tests/clips.rs` y `sample_cache.rs`), más tests nuevos junto al código: impulso por pares de frecuencias (D1); unión de dos clips contiguos de seno continuo con desviación < 1e-6 (D4); render offline con clic en la muestra 0 igual al archivo (D6); warp ≡ tempo con residuo < −120 dB en el interior (D5); inicio fraccionario con frame 0 presente (D8). `cargo check --workspace` y `cargo clippy -p aitrack-engine` al terminar.

---

## Anexo · Resultados de la sonda (resumen)

Prueba de nulo, estéreo 48 kHz `f32`, 4 s, render con el grafo real (pista de audio a 0 dB, pan 0, Main):

| Caso | Residuo | Observación |
|---|---|---|
| A, sin warp, 120 BPM, clip en tick 0 | −36,3 dB sobre toda la señal | cabeza: ×0,00098 en muestra 0 (D6); cola: ×1/64 en la última (D4) |
| A2, mismo, interior | máx. 1,19e-7 (−138,5 dBFS); −135 dB | 1 ulp de `pan_gains` |
| B, 128 BPM / 126 BPM / 140 BPM, inicio fraccionario | −37…−40 dB | frame 0 perdido en 126 BPM (D8) |
| C, mono | igual que A | mono→L=R a 0 dB |
| D, PCM 16 y 24 bits | igual que A | decodificación exacta |
| E, warp ON `original == proyecto` (Beats), interior | −30,3 dB (señal de prueba), −17,8 dB ruido blanco, −35,1 dB ruido rosa | D5 |
| F, offset 480 ticks, interior | −135 dB | exacto |
| SRC (44,1→48, bombo sintético) vs referencia sinc independiente | **+3,2 dB** (real) frente a **−46,8 dB** (sin `drain`) | D1 |
