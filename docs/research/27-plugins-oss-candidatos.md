# 27 - Plugins open source y gratuitos de calidad: Waves/Cymatics, candidatos nuevos y revisión del catálogo

Fecha: 2026-11-21 · Alcance: investigación web + GitHub sobre `content/plugins/manifest.toml` (45 entradas) · No se descargó ni instaló ningún binario (solo páginas HTML, metadatos de la API de GitHub y cabeceras HEAD) · No se tocó código, manifest ni git.

Convención de evidencia:
- **[V]** comprobado el 2026-11-21 en la página oficial, en la API de GitHub o con una petición HEAD.
- **[H]** comprobado en el host del dueño (informe 16, 2026-11-15). Es la versión que tenía instalada, no necesariamente la actual.
- **[S]** supuesto o fuente secundaria (prensa, tiendas, resúmenes). **NC** = no confirmado.
- Aviso metodológico: las páginas se leyeron con una herramienta que resume el contenido (no con el HTML crudo). Solo se leyeron cláusulas de la licencia de Waves y de TAL; el resto de EULA no se leyó. Valhalla devuelve 403 a los clientes automáticos e iZotope 429. Esto no es asesoría legal.

> Las URL de los bloques de §8 salen de la API de GitHub o de la página del fabricante y se comprobaron con HEAD (§8). Los datos del host del dueño [H] vienen del informe 16 y corresponden a las versiones que tenía instaladas.

---

## 0. Resumen ejecutivo

1. **Waves y Cymatics no tienen versión open source.** Lo gratis que dan (Waves Free Plugin Pack; Cymatics Diablo Lite, Space Lite y Origin) es software propietario con licencia de usuario final, sin código fuente, ligado a una cuenta o a un formulario con correo, y con formatos que AI-Track casi no aloja (Cymatics: VST2/AU/AAX sin VST3 confirmado). No se pueden incluir ni espejar, y copiar su DSP, nombres o aspecto está vetado por PLAN.md §9 riesgo 10 y por el informe 04 §8. Lo que sí vale: plugins libres con la misma función (§2.4).
2. **Hay 29 plugins open source de calidad que no están en el manifest** (19 ya probados en el host del dueño, informe 16, y 10 sin probar), todos con zip, dmg o pkg públicos en GitHub salvo Airwindows y Monique, que son rodantes. Los mejores para electrónica: ZL Equalizer 2 y ZL Compressor, GATE-12, TIME-12 y FILT-R (forma rítmica, stutter y movimiento de filtro, el hueco que el informe 17 §3.1 marca en `crates/stock`), JC303 (bajo ácido), ChowTapeModel y BYOD (saturación), Dragonfly, REEV-R y CloudReverb (reverb), OB-Xf, ChowKick y Airwindows.
3. **Del catálogo actual, 16 de 16 artefactos fijados siguen vivos** con el tamaño exacto fijado (HEAD, §5). Lo que cambió: OB-Xd quedó congelado y pasó a Retromulator (sustituir por OB-Xf), Spitfire LABS legacy terminó el 2026-10-31, cuatro entradas TAL pueden pasar de enlace a descarga directa en macOS, y varias páginas cambiaron de URL (§5.2).
4. **Reglas aplicadas:** nada de espejos propios; solo descarga directa del fabricante con sha256 fijado (`cargo xtask plugins pin`); los plugins GPL/AGPL solo con descarga iniciada por el usuario y nunca en el instalador MIT. El instalador actual solo abre `zip`, `tar_gz`, `dmg` y `pkg`: los instaladores `.exe` y `.msi` de Windows (ZL, ChowDSP, Geonkick) obligan a que Windows sea enlace guiado (§9).

### Top 15 recomendados (nuevos, por valor para un productor de electrónica)

| # | Plugin | Licencia | Categoría | Formato | Plataformas (descarga) | Carril |
|---|---|---|---|---|---|---|
| 1 | ZL Equalizer 2 1.4.2 | AGPL-3.0 | EQ dinámico + analizador | VST3 [H] | macOS dmg arm64 y x86-64 · Windows solo msi | directo (macOS), Windows enlace |
| 2 | GATE-12 1.3.3 | GPL-3.0 | puerta rítmica / volume shaper (bombeo tipo sidechain) | VST3 [H] | zip macOS y Windows | directo |
| 3 | TIME-12 1.2.3 | GPL-3.0 | stutter, tape stop, glitch | VST3 | zip macOS y Windows | directo (probar antes) |
| 4 | FILT-R 1.3.0 | AGPL-3.0 | filtro con envolvente dibujada | VST3 [H] | zip macOS y Windows | directo |
| 5 | JC303 0.14.0 | GPL-3.0 | bajo ácido TB-303 | VST3 + CLAP [H] | zip macOS universal y Windows x64 | directo |
| 6 | ZL Compressor 0.6.1 | AGPL-3.0 | compresor de bus | VST3 [H] | dmg arm64/x86-64 · Windows msi | directo (macOS) |
| 7 | ChowTapeModel 2.11.4 | GPL-3.0 | cinta / saturación | VST3 + CLAP [H] | dmg macOS · Windows exe | directo (macOS) |
| 8 | BYOD 1.3.0 | GPL-3.0 | distorsión modular | VST3 + CLAP [H] | dmg macOS · Windows exe | directo (macOS) |
| 9 | Dragonfly Hall/Room/Plate 3.2.10 | GPL-3.0 | reverb | VST3 + CLAP [H] | dmg universal · zip Windows x64 | solo enlace (el instalador copiaría Early Reflections, que cuelga; usar sandbox) |
| 10 | REEV-R 1.4.0 | GPL-3.0 | reverb de convolución con modulación | VST3 [H] | zip macOS y Windows | directo |
| 11 | OB-Xf 1.0.3 | GPL-3.0 | sinte tipo OB-X | VST3 + CLAP [H] | dmg macOS · zip Windows | directo |
| 12 | ChowKick 1.2.0 | BSD-3-Clause | bombo / 808 sintético | VST3 + CLAP [H] | dmg macOS · Windows exe | directo (macOS) |
| 13 | CloudReverb 0.5 | GPL-3.0 | reverb algorítmico (CloudSeed) | VST3 [H] | zip macOS y Windows x64 | directo |
| 14 | Airwindows Consolidated (rodante) | MIT (código) / GPL-3.0 (binario) | ~500 efectos (de-esser, cinta, consola) | VST3 + CLAP [H] | dmg macOS · zip Windows | solo enlace (nombre con fecha y hash) |
| 15 | Wolf Shaper 1.0.2 | GPL-3.0 | waveshaper con editor de curva | VST3 + CLAP [H] | zip universal macOS · zip Windows x64 | directo (DPF: sandbox) |

Menciones: RipplerX (sinte de modelado físico), ChowMatrix (delay), ChowCentaur (overdrive), ZL Splitter, TDR Molotok (gratis cerrado, compresor), cuatro TAL gratis que pueden pasar a directo en macOS.

### Lo que murió o cambió en el catálogo actual

- **OB-Xd:** el standalone se congela en 2.20 ("free for any use, including commercial work", licencia "Freeware", sin permiso de redistribución); las novedades pasan a Retromulator 2.2.0, producto de discoDSP (noviembre de 2026) [V]. Sustituir por **OB-Xf** (GPL-3.0, activo, probado en el host).
- **Spitfire LABS:** fin de soporte el 2026-10-31; el contenido pasa a Splice INSTRUMENT, gratis con cuenta Splice (AU, VST3, AAX; sin CLAP) [V].
- **Voxengo SPAN:** en macOS solo AU/AAX/VST2 (el dmg se llama `..._Mac_AU_AAX_VST_setup.dmg`); VST3 solo en Windows [V]. Inútil en Mac.
- **Analog Obsession:** la página solo lista macOS 10.11 a 13.x y dice que "acaba de empezar" a actualizar a Apple Silicon [V]. Caduco.
- **Cymatics:** sin VST3 ni CLAP confirmados [S]; entrada a descartar.
- URLs movidas: Klanghelm MJUC, Wavesfactory, Softube, Slate, Analog Obsession (§5.2).
- Nada ha muerto: 16 de 16 URL fijadas responden 200 con el tamaño fijado.

---

## 1. Qué cuenta como "open source" y qué como "gratis cerrado"

| Clase | Qué es | Ejemplos de esta lista | Qué hace AI-Track |
|---|---|---|---|
| Open source | Licencia aprobada por la OSI y código público | ZL Audio, tiagolr, ChowDSP, Dragonfly, JC303, OB-Xf, CloudReverb, Wolf Shaper, Surge XT, Dexed | Descarga directa desde la release del proyecto con sha256 fijado. GPL/AGPL: solo a petición del usuario, nunca empaquetado |
| Híbrido | Código GPL, pero el binario oficial lo distribuye el autor con otras condiciones | Vital (código GPL-3.0; binario gratuito con cuenta, sin usar el nombre en builds propios ni redistribuir los presets gratuitos [V, README de `mtytel/vital`]); OB-Xd de discoDSP ("Freeware") | Solo enlace |
| Gratis cerrado (freeware) | Binario sin fuente; gratis, a veces con cuenta | TDR (Nova, Kotelnikov, SlickEQ, Molotok), TAL, u-he, Klanghelm, Valhalla, Voxengo, Youlean, Kilohearts, Decent Sampler, Splice INSTRUMENT, Waves, Cymatics | Descarga del servidor del fabricante solo si el EULA no lo impide y sin cuenta; si no, enlace guiado |

---

## 2. Waves y Cymatics: la verdad

### 2.1 Waves Audio

| Pregunta | Respuesta |
|---|---|
| ¿Hay algo open source? | No. No se encontró ningún repositorio ni código publicado de sus procesadores. |
| ¿Qué dan gratis? | **Waves Free Plugin Pack** (abril 2025): siete plugins (Lil Tube, IR Live Reverb, V-EQ3, V-Comp, AudioTrack, Flow Motion, GTR Solo) más dos de StudioVerse, en AAX, VST3 y AU para macOS y Windows [S, MusicRadar]. A abril de 2026 la web de Waves seguía promocionándolo [S]. Además hay regalos temporales: un plugin gratis por Black Friday 2026 (alta de aviso abierta, disponible el viernes 2026-11-27 y hasta el 30/11/26, "número limitado de licencias", plugin aún sin anunciar) [V, Production Expert] y Curves Resolve gratis 48 horas en enero de 2026 [S]. |
| ¿Cómo se obtiene? | Cuenta Waves + Waves Central; el plugin se "deposita" en la cuenta. La página de la oferta (`waves.com/account/free-plugin-pack`) no se pudo leer sin sesión. |
| ¿Bajo qué licencia? | EULA de Waves, "Last updated: September 2026" [V, https://www.waves.com/legal/eula]. Licencia "limitada, mundial, no exclusiva, no transferible"; uso para fines internos personales o de negocio; **una licencia por dispositivo** a la vez (máquinas virtuales y contenedores cuentan como dispositivos distintos). Prohíbe: ingeniería inversa, descompilar o intentar descubrir "source code, data representations, underlying algorithms, processes, methods"; crear obras derivadas; alojar el software como servicio; redistribución y sublicencia comerciales; automatizar el acceso. "Some Software requires an active Account and periodic internet communications for activation and License verification". |
| ¿Por qué no se puede incluir? | (1) El EULA prohíbe redistribuir y sublicenciar. (2) La activación depende de cuenta e internet. (3) No hay código: cualquier clon sería ingeniería inversa, vetada por el EULA. (4) Copiar su DSP, nombres, presets o aspecto contradice PLAN.md §9 riesgo 10 ("Marcas y trade dress... Nombres y diseño visual propios") y el informe 04 §8 ("Qué NO copiar"). |
| Qué ya hace el catálogo | `waves-free-pack` es `lane = "link"` con `requires_account = true`: abre la página oficial y reconoce el plugin tras instalarse. Correcto; no cambiar. |

### 2.2 Cymatics

| Pregunta | Respuesta |
|---|---|
| ¿Hay algo open source? | No. Cymatics es sobre todo una tienda de packs de samples; sus plugins son propietarios. |
| ¿Qué dan gratis? | **Diablo Lite** (realzador de batería: "Punch" = transient shaper y "Clip" = clipper duro/suave) [V, cymatics.fm/pages/diablo-lite]; **Space Lite** (reverb, "VST/AU", "64 bit only"; el Space completo tiene más de 20 modos y se regala con la compra de una colección) [V, cymatics.fm/products/space-lite-plugin]; **Origin** (efecto lo-fi vintage con remuestreo/bitcrusher) [S, reseñas de MusicRadar y Bonedo; la ficha oficial no se pudo leer por 429]. |
| ¿Cómo se obtiene? | Formulario con nombre y correo ("Complete the form below to download your free plugin") o la app **Cymatics Hub** (instaladores para Mac y Windows) [V/S]. |
| ¿Formatos? | Las fichas no los detallan; las reseñas hablan de VST, AU y AAX. **No hay evidencia de VST3 ni de CLAP**, así que AI-Track, que aloja VST3 y CLAP, probablemente no los cargue [S]. |
| ¿Licencia? | Sin EULA de plugin visible. Los Términos de uso (última actualización 2020-12-01) no tratan licencias de software; hablan de uso personal no comercial del sitio y prohíben reproducir o distribuir su contenido [V, cymatics.fm/pages/terms-of-use]. "All Cymatics packs are 100% Royalty Free" se refiere a los packs de samples, no a los plugins [V, página de un bundle]. |
| ¿Por qué no se pueden incluir? | Sin licencia de redistribución, con formulario/cuenta, sin código y sin formato alojable confirmado. Misma regla de marcas y trade dress. |
| Recomendación | **Descartar** `cymatics-free` del catálogo (o dejarlo como enlace sin promesa, `formats = []`, como hoy). Su función se cubre mejor con ChowKick, JC303, BYOD, Wolf Shaper y los efectos propios. |

### 2.3 Regla de marcas aplicada a este informe

Los nombres de productos comerciales aparecen aquí solo para describir una función (informe de investigación interno). Ninguno debe usarse en nombres de dispositivos propios, presets o textos de la app (PLAN.md §9 riesgo 10; informe 04 §8.1).

### 2.4 Mapa de funciones: qué plugin libre cubre cada una

| Función por la que son conocidos | Libre / open source (verificado) | Gratis cerrado | Propio en `crates/stock` (informe 17 §3) |
|---|---|---|---|
| Compresor / glue de bus | ZL Compressor (AGPL-3.0) | TDR Kotelnikov, TDR Molotok, Klanghelm MJUC jr y DC1A | `fx.compressor` (Clean, Punch, Glue, Opto): cubierto |
| EQ de bus y dinámico | ZL Equalizer 2 (AGPL-3.0), ChowMultiTool (GPL-3.0) | TDR Nova, TDR SlickEQ | `fx.eq`: cubierto |
| Limitador de master | ZL Limiter 0.0.1 (AGPL-3.0, **prerelease**) | LoudMax (VST3 en Mac y Windows) | `fx.limiter`, `fx.clipper`: cubierto |
| Saturación, cinta, distorsión | ChowTapeModel, BYOD (GPL-3.0); ChowCentaur (BSD-3-Clause); Wolf Shaper (GPL-3.0); Airwindows | Klanghelm IVGI | `fx.saturator`, `fx.shaper`: cubierto |
| Reverbs | Dragonfly, CloudReverb, REEV-R (GPL-3.0) | Valhalla Supermassive, TAL-Reverb-4 | `fx.reverb`: cubierto (aportan carácter) |
| Delays | ChowMatrix (BSD-3-Clause), QDelay y SIRIAL (GPL-3.0) | Valhalla FreqEcho | parcial |
| De-esser | Airwindows (DeEss, dentro de Consolidated); ZL Equalizer 2 (dinámico) | TDR Nova (EQ dinámico paralelo) | **sin dispositivo dedicado** |
| Pitch / vocal / afinador | **No hay libre en VST3/CLAP para Mac**: x42-autotune es solo LV2 | TAL-Vocoder-2 (vocoder); Graillon 3 Free (formatos listados: VST/AAX/AU/LV2) | **hueco** |
| Sintes y bajos 808, kicks | JC303 (303), OB-Xf, Surge XT, Dexed, RipplerX, TetraOp (GPL-3.0); ChowKick (BSD-3-Clause) | Zebralette 3, Podolski, TAL-NoiseMaker, Vital Basic (cuenta) | `instrument.kick808`, `instrument.acid303`, `instrument.wavetable`: cubierto |
| Risers, FX, glitch, stutter | TIME-12, GATE-12, FILT-R (rítmicos); Surge XT Effects; los risers son sobre todo samples (banco de sonidos) | Glitch² (gratis), FAULTLINE (cuenta), Kilohearts Essentials (Tape Stop, Trance Gate) | **hueco** (stutter, beat repeat, half-time, tape stop) |
| Multibanda tipo OTT | ZL Splitter (solo reparte señal, no comprime) | Xfer OTT, Audio Damage Evil Otto | `fx.multiband` solo crossover: **hueco** |
| Medidores de loudness | (ninguno libre alojable: x42-meters es LV2) | Youlean Loudness Meter 2 (VST3) | `fx.analyzer`: cubierto |

---

## 3. Reglas de decisión usadas

| Carril | Cuándo | Condición |
|---|---|---|
| **instalador directo** (`lane = "direct"`) | Descarga pública del propio fabricante o proyecto, sin cuenta, en `zip`, `tar_gz`, `dmg` o `pkg`, con nombre estable por versión | sha256 y tamaño fijados con `cargo xtask plugins pin`; GPL/AGPL solo a petición del usuario |
| **solo enlace** (`lane = "link"`) | Cuenta, formulario, gestor propio, instalador `.exe`/`.msi`/`7z`, nombre rodante (fecha/hash), host compartido (Dropbox, S3) | Se abre la página oficial y se reconoce el plugin tras instalarse |
| **descartar** | Sin VST3/CLAP, sin binario para macOS o Windows, abandonado, inestable en el host o de pago | Se anota el motivo |

Un artefacto solo de macOS con `lane = "direct"` es válido (precedente: `tal-noisemaker`); la tarjeta ofrece Windows como enlace guiado.

---

## 4. Candidatos nuevos (no están en el manifest)

Orden por valor para un productor de electrónica. Las columnas de "Señales de estabilidad" combinan lo probado en el host [H], el framework y los issues abiertos de GitHub (el contador incluye pull requests).

### 4.1 Open source ya probados en el host del dueño [H]

| # | Plugin y versión | SPDX [V] | Formatos | macOS | Windows x64 | Descarga y tamaño [V] | Estabilidad | Recomendación |
|---|---|---|---|---|---|---|---|---|
| A1 | **ZL Equalizer 2** 1.4.2 (2026-11-09) | AGPL-3.0 | VST3 [H]; AU y LV2 en README [S] | dmg `arm64` 11,4 MB y `x86-64` 12,9 MB (no universal) | solo `.msi` 10,4 MB (+AVX2 10,7; arm64 9,4) | GitHub `ZL-Audio/ZLEqualizer` | JUCE (fork propio); 610 parámetros; editor abre y cierra [H]; 1 issue abierto | **directo** macOS (dos artefactos por arquitectura); Windows enlace |
| A2 | **ZL Compressor** 0.6.1 (2026-09-28) | AGPL-3.0 | VST3 [H] | dmg arm64 12,5 MB; x86-64 14,1 MB | msi 10,9 MB | GitHub `ZL-Audio/ZLCompressor` | 83 parámetros [H]; el sidechain externo no llega (informe 16 P4); 1 issue | **directo** macOS; Windows enlace |
| A3 | **ZL Splitter** 0.4.0 (2026-11-07) | AGPL-3.0 | VST3 [H] | dmg arm64 5,3 MB; x86-64 5,9 MB | msi 5,7 MB | GitHub `ZL-Audio/ZLSplitter` | 16 parámetros [H]; 1 issue | **directo** macOS |
| A4 | **GATE-12** 1.3.3 (2026-01-21) | GPL-3.0 | VST3 [H]; AU (Mac) y LV2; CLAP "planeado" | zip 12,5 MB | zip 6,4 MB | GitHub `tiagolr/gate12` | JUCE; README: builds de macOS "untested and unsigned" (nuestro instalador firma ad hoc); 30 parámetros, editor OK [H]; 1 issue | **directo** |
| A5 | **FILT-R** 1.3.0 (2026-07-06) | AGPL-3.0 | VST3 [H]; AU y LV2 | zip 13,4 MB | zip 6,6 MB | GitHub `tiagolr/filtr` | JUCE; 57 parámetros, latencia 49 [H]; 5 issues | **directo** |
| A6 | **REEV-R** 1.4.0 (2026-01-22) | GPL-3.0 | VST3 [H]; AU y LV2 | zip 19,3 MB | zip 7,7 MB | GitHub `tiagolr/reevr` | JUCE + biblioteca de convolución KlangFalter; **no trae respuestas al impulso** salvo la de por defecto (README); 107 parámetros [H]; 5 issues | **directo**; avisar de que hay que instalar IR |
| A7 | **JC303** 0.14.0 (2026-11-14) | GPL-3.0 (proyecto entero); parte del DSP es MIT y parte AGPL-3.0-or-later [V, README] | VST3, CLAP, LV2 y AU en la release; VST2 solo compilando; VST3 + CLAP [H] | zip universal 69,3 MB | zip x64 27,4 MB | GitHub `midilab/jc303` | JUCE (puerto CMake de Open303) [V]; 40 parámetros [H]; versión muy reciente y activa; 13 issues | **directo** |
| A8 | **OB-Xf** 1.0.3 (2026-03-27) | GPL-3.0 | VST3 + CLAP [H] | dmg 60,7 MB | zip 9,2 MB (+arm64) | GitHub `surge-synthesizer/OB-Xf` | Equipo de Surge; firma Team `547U7G7MZC` con runtime endurecido [H]; imprime aviso de condición de carrera del parche por defecto [H]; 5 issues | **directo**; sustituye a `obxd` |
| A9 | **CloudReverb** 0.5 (2026-04-14) | GPL-3.0 | VST3 [H] | zip 15,1 MB | zip 8,2 MB | GitHub `xunil-cloud/CloudReverb` | Proyecto joven (0.x), basado en CloudSeed; 47 parámetros [H]; 4 issues | **directo** |
| A10 | **Dragonfly Reverb** 3.2.10 (2023-04-23) | GPL-3.0 | VST3 + CLAP [H] | dmg universal 25,0 MB | zip x64 22,4 MB | GitHub `michaelwillis/dragonfly-reverb` | **DPF**; Hall, Room y Plate OK [H]; **Early Reflections cuelga** (descartado); las clases Objective-C `DGL_Pugl…` chocan entre plugins DPF cargados juntos (informe 16 P6): usar sandbox; bundles sin `Info.plist` [H]; 31 issues | **solo enlace** hasta poder excluir bundles (§9): el instalador copia todos los `.vst3`/`.clap` del paquete, también Early Reflections |
| A11 | **Wolf Shaper** 1.0.2 (2023-05-14) | GPL-3.0 | VST3 + CLAP [H] | zip universal 6,0 MB | zip x64 5,5 MB | GitHub `wolf-plugins/wolf-shaper` | **DPF**, prefijo propio en clases ObjC [H]; 11 parámetros; 52 issues; último push 2023-11 | **directo** (sandbox) |
| A12 | **BYOD** 1.3.0 (2024-03-16) | GPL-3.0 | VST3 + CLAP [H] (README: AU, LV2) | dmg 89,1 MB | solo `.exe` 26,4 MB | GitHub `Chowdhury-DSP/BYOD` | JUCE; 509 a 511 parámetros [H]; firma Team `46MK76T24B` [H]; 54 issues | **directo** macOS |
| A13 | **ChowTapeModel** 2.11.4 (2023-11-05) | GPL-3.0 | VST3 + CLAP [H] | dmg 28,7 MB | `.exe` 10,6 MB | GitHub `jatinchowdhury18/AnalogTapeModel` | JUCE; latencia 39/37 muestras [H]; 41 issues | **directo** macOS |
| A14 | **ChowKick** 1.2.0 (2023-01-16) | BSD-3-Clause | VST3 + CLAP [H] | dmg 18,9 MB | `.exe` 5,8 MB | GitHub `Chowdhury-DSP/ChowKick` | JUCE; pico > 1 (ganancia propia) [H]; 11 issues; sin cambios desde 2023 | **directo** macOS |
| A15 | **ChowMatrix** 1.3.0 (2021-12-21) | BSD-3-Clause | VST3 [H] | dmg 17,1 MB | `.exe` 8,0 MB | GitHub `Chowdhury-DSP/ChowMatrix` | 667 parámetros [H]; el escáner murió una vez al salir por `process::exit` (informe 16 §3.4, ya corregible); 26 issues | **directo** macOS |
| A16 | **ChowCentaur** 1.4.0 (2021-05-26) | BSD-3-Clause | VST3 [H] | dmg 12,3 MB | `.exe` 9,0 MB | GitHub `jatinchowdhury18/KlonCentaur` | 7 parámetros [H]; 9 issues; sin cambios desde 2021 | **directo** macOS |
| A17 | **Airwindows Consolidated** (release rodante) | MIT el código de Airwindows; **el binario DAW arrastra JUCE y el SDK VST3 bajo GPL-3.0** (informe 17) | VST3 + CLAP [H] | dmg ~49,6 MB (`airwindows-consolidated-macOS-2026-11-15-59a227e.dmg`) | zip 13,6 MB (`AirwindowsConsolidated-2026-11-15-59a227e-Windows.zip`) | GitHub `baconpaul/airwin2rack`, tag `DAWPlugin` (se reescribe cada semana) | 14 parámetros [H] (un selector de efecto); firma Team `547U7G7MZC` [H]; 47 issues | **solo enlace**: el nombre lleva fecha y hash, no se puede fijar |
| A18 | **sfizz** 1.2.3 (2024-01-14) | BSD-2-Clause | VST3 [H] (sin CLAP) | pkg 11,9 MB (release de `sfizz-ui`) | solo `.exe` 3,6 MB | GitHub `sfztools/sfizz-ui` | **repositorio archivado** (2025); suena un seno por defecto sin SFZ [H]; 73 issues | solo enlace o directo macOS con baja prioridad: AI-Track ya tiene `instrument.sfz` |
| A19 | **Monique Monosynth** (build 2024-08-07) | GPL-3.0 o MIT (doble licencia del código); los binarios usan la compilación GPL-3.0 de JUCE [V, README] | VST3 + CLAP [H] | dmg 26,2 MB | zip 11,5 MB | GitHub `surge-synthesizer/monique-monosynth`, tag rodante `Nightly` | Solo "Nightly"; repo sin commits desde 2024-07; primer `prepare` con pico bajo (informe 16 P8) | **solo enlace** (sin versión estable) |

Indicio adicional: issues abiertos en GitHub que mencionan `crash` y `hang`/`freeze` (búsqueda por palabra, 2026-11-21 [V]; no sustituye a la prueba en el host). ZL Equalizer 2, ZL Compressor, CloudReverb y Wolf Shaper: 0 y 0. GATE-12, FILT-R, REEV-R, TIME-12 y OB-Xf: 1 y 0. JC303: 2 y 1. RipplerX: 1 y 1. Airwindows: 2 y 2. Dragonfly: 2 y 3 (coincide con el cuelgue de Early Reflections). ChowTapeModel: 4 y 2. BYOD: 10 y 4 (proyecto grande y muy usado).

### 4.2 Open source nuevos, aún sin probar en el host

| # | Plugin y versión | SPDX [V] | Formatos | macOS | Windows x64 | Descarga y tamaño [V] | Estabilidad | Recomendación |
|---|---|---|---|---|---|---|---|---|
| B1 | **TIME-12** 1.2.3 (2026-01-11) | GPL-3.0 | VST3, AU, LV2 | zip 12,3 MB | zip 5,5 MB | GitHub `tiagolr/time12` | Mismo autor y motor que GATE-12 y FILT-R (que pasaron); la descripción dice stutter, tape stop, glitch, scratch, reverse y pitch shift; 2 issues | **directo** tras una prueba de carga/editor |
| B2 | **RipplerX** 1.5.19 (2026-06-13) | GPL-3.0 | VST3, AU, LV2 | zip 15,6 MB | zip 6,9 MB | GitHub `tiagolr/ripplerx` | JUCE; sinte de modelado físico; 664 estrellas; 14 issues | **directo** tras prueba |
| B3 | **TetraOp** 1.0.3 (2026-07-12) | GPL-3.0 | NC (JUCE) | zip 14,0 MB | zip 7,1 MB | GitHub `tiagolr/tetraop` | Sinte FM de 4 operadores con wavetables; v1.0 reciente; 17 issues | probar antes |
| B4 | **QDelay** 1.2.3 (2026-09-16) | GPL-3.0 | NC | zip 8,9 MB | zip 3,3 MB | GitHub `tiagolr/qdelay` | Delay doble; 11 issues | probar antes |
| B5 | **SIRIAL** 1.1.1 (2026-03-22) | GPL-3.0 | NC | zip 7,9 MB | zip 2,8 MB | GitHub `tiagolr/sirial` | Delay rítmico; 1 issue | probar antes |
| B6 | **ChowMultiTool** 1.1.0 (2024-02-13) | GPL-3.0 | VST3/CLAP NC | dmg 30,6 MB | `.exe` 7,4 MB | GitHub `Chowdhury-DSP/ChowMultiTool` | EQ, waveshaper, divisor; 9 issues | probar antes |
| B7 | **ZL Spectrum Equalizer** 0.1.2 (2026-11-09) | AGPL-3.0 | NC | dmg 5,8/6,7 MB | msi 6,1 MB | GitHub `ZL-Audio/ZLSpectrumEqualizer` | 0.1.x: demasiado joven | esperar |
| B8 | **ZL Limiter** 0.0.1 (2026-10-31) | AGPL-3.0 | NC | dmg 5,0/5,7 MB | msi 5,6 MB | GitHub `ZL-Audio/ZLLimiter` | **Prerelease** | esperar a 0.1 |
| B9 | **Cardinal** 26.02 (2026-02-28) | GPL-3.0 | VST3, CLAP, AU, LV2 [S] | pkg universal **974 MB** | zip 1,1 GB / instalador 369 MB | GitHub `DISTRHO/Cardinal` | 161 issues; módulos con licencias propias (informe 17) | **solo enlace** (tamaño) |
| B10 | **Odin 2** 2.4.1 | GPL-3.0 (fuente Aldrich bajo OFL-1.1) [V, LICENSE] | VST3, CLAP, AU | pkg 132 MB | instalador `.exe` 41 MB | GitHub `TheWaveWarden/odin2` | **Descartado por el dueño** (necesita un workaround); 61 issues | descartar hasta volver a probarlo (§9: la firma ad hoc automática puede ser ese workaround) |

### 4.3 Open source descartados

| Proyecto | SPDX | Motivo [V] |
|---|---|---|
| LSP Plugins 1.2.37 | LGPL-3.0 | La web oficial no ofrece binarios para macOS (solo compilar); Windows en un sitio de builds (lsp-builds.com) sin operador identificado. Además `LGPL-3.0` no está en `ALLOWED_LICENSES` |
| x42 plugins | GPL-2.0 | "Available as LV2-plugin and JACK-application": ni VST3 ni CLAP. x42-autotune y x42-meters serían el afinador y el medidor, pero no se alojan |
| Geonkick 3.9.1 | GPL-3.0 | Binarios solo para Windows (instalador `.exe` de la versión "basic": 1 instrumento) y Linux; sin macOS |
| Vitalium / DISTRHO-Ports | GPL | El repositorio solo conserva `vitalium` y `syndicate` como fuentes para Linux/LV2; sin binarios de macOS ni Windows. Las versiones libres antiguas de TAL ya no están allí |
| Helm | GPL-3.0 | Repositorio archivado desde 2022 |
| Wolf Spectrum | GPL-3.0 | Solo VST2 y LV2/JACK |
| plugdata 0.9.3 | GPL-3.0 | 770 MB (pkg) y msi en Windows; es un entorno de programación, no un efecto |
| BespokeSynth 1.3.0 | GPL-3.0 | Aplicación independiente, no plugin |
| Ildaeil 1.3 | GPL-2.0 | Plugin que aloja otros plugins: duplica nuestro host |
| DPF-Plugins 1.7 | NOASSERTION | Paquete de varios plugins de 2023 con licencias mixtas |

### 4.4 Gratis pero cerrado: candidatos nuevos y los que pediste revisar

| Plugin | Estado verificado [V] | Recomendación |
|---|---|---|
| **TDR Molotok** 1.0.7 | Gratis ("Free Download"); VST3 en Mac de 64 bits y Windows de 64 bits; zips públicos en `tokyodawn.net` con el mismo patrón que Nova y Kotelnikov. La ficha **no dice si el Mac es Apple Silicon** | directo si `lipo -archs` al fijar el hash muestra `arm64`; si no, descartar |
| **TAL-Reverb-4** 4.0.4, **TAL-Chorus-LX** 1.6.3, **TAL-Filter-2** 3.1.2, **TAL-Vocoder-2** 3.0.4 | La página de productos de TAL los lista bajo "FREE Plug-Ins"; VST3 + CLAP + AU + AAX, Universal Binary 2; zip de macOS público con el instalador `.pkg` dentro (patrón de `tal-noisemaker`); el zip de Windows es un instalador. Cláusulas: 7 prohíbe redistribuir, 11 prohíbe la ingeniería inversa, 13 permite el uso comercial de los productos gratuitos [V, tal-software.com/disclaimer] | **pasar de enlace a directo solo en macOS** (§8.2) |
| **Glitch²** 2.1.5 (illformed) | "Completely free", en memoria de Kieran; VST3 en Windows, macOS y Linux, AU en Mac; zip directo por plataforma sin cuenta; sin EULA y sin fecha de fin; "All rights reserved" | **directo**: zips por plataforma verificados con HEAD (§8.4); riesgo: la gratuidad no se declara permanente |
| **Kilohearts Essentials** | "Free with an account"; 34 productos; incluye Tape Stop, Trance Gate, Formant Filter, Pitch Shifter, Reverb, Limiter; instalador y cuenta de Kilohearts | solo enlace (como hoy) |
| **Voxengo SPAN** 3.24 | macOS: `VoxengoSPAN_324_Mac_AU_AAX_VST_setup.dmg` (sin VST3); Windows: `..._Win32_64_VST_VST3_AAX_setup.exe` | solo enlace y solo útil en Windows |
| **Valhalla** Supermassive 5.0.0, FreqEcho, Space Modulator | Siguen gratis según varias listas de 2026; VST3, AU y AAX, sin CLAP; sin iLok ni registro [S]; la web devuelve 403 a clientes automáticos | solo enlace |
| **Spitfire LABS** / **Splice INSTRUMENT** | LABS y LABS+ sin soporte desde el 2026-10-31; INSTRUMENT es gratis con cuenta Splice (AU, VST3, AAX, sin CLAP; macOS 12+ con Apple Silicon, Windows 10+) | solo enlace |
| **Decent Sampler** 1.43.2 | Gratis con formulario de correo; VST, VST3, AAX, AU; Mac Intel y ARM; sin CLAP | solo enlace |
| **Vital** (Basic) | Planes Basic (75 presets, 25 wavetables), Plus y Pro; macOS 10.15+ Intel o Apple Silicon, Windows 10+; cuenta en `account.vital.audio`. Código GPL-3.0, binario con otras condiciones (§1) | solo enlace |

---

## 5. Revisión de las 45 entradas del manifest

### 5.1 Resultado de las comprobaciones

- **Artefactos fijados (16 de 16, en 9 entradas `direct`):** HEAD a cada URL: todas devuelven 200 y su `Content-Length` coincide exactamente con el `size` fijado [V]. Fechas de los ficheros: TDR Nova 2026-10-12 (misma versión 2.2.3: conviene que `cargo xtask plugins verify` confirme el hash), Kotelnikov 2026-03, SlickEQ 2025-11 y 2025-02, Youlean 2026-10-21, TAL-NoiseMaker 2024-08, Zebralette 3 2025-12-10, Podolski 2025-08, Surge XT 2024-08-11, Dexed 2026-10-31.
- **Páginas de inicio:** 42 de 45 responden 200; las tres de Valhalla devuelven 403 a clientes automáticos (el sitio bloquea bots, no está caído) [V].
- **Versiones:** TDR Nova 2.2.3, Kotelnikov 1.6.7, Youlean 2.5.15, TAL-NoiseMaker 5.0.6, Podolski 1.2.3, Dexed 1.1.0 y Surge XT 1.3.4 (última estable de `releases-xt`, 2024-08-11; existen nightlies) siguen siendo las vigentes [V].

### 5.2 Entradas que cambiaron o murieron

| Entrada | Qué cambió [V] | Acción |
|---|---|---|
| `obxd` | El standalone queda en 2.20; OB-Xd 2 pasa a Retromulator 2.2.0 (2026-11); licencia "Freeware", "free for any use, including commercial work"; redistribución no dicha | Sustituir por `ob-xf` (§8.1) |
| `splice-instrument` | Spitfire LABS sin soporte desde 2026-10-31; sin CLAP | Actualizar descripción; `formats = ["vst3"]` |
| `voxengo-span` | En macOS no hay VST3 | Mantener como enlace y anotar "solo Windows" |
| `analogobsession` | Página: macOS 10.11 a 13.x; Apple Silicon "acaba de empezar" | Anotar o retirar |
| `cymatics-free` | Sin VST3/CLAP confirmado | Retirar |
| `klanghelm-mjuc-jr` | `https://klanghelm.com/MJUC` redirige a `/contents/products/MJUC` | Corregir `homepage` |
| `wavesfactory-free` | Redirige a `/free-vst-plugins/` | Corregir `homepage` |
| `softube-saturation-knob` | Redirige a `/plug-ins/saturation-knob`; sigue gratis con Softube Central, cuenta Softube e iLok (sin llave física), tres activaciones; AU, VST, VST3, AAX | Corregir `homepage` |
| `slatedigital`, `analogobsession` | Dominio sin `www` | Corregir `homepage` |
| `tal-reverb-4`, `tal-chorus-lx`, `tal-filter-2`, `tal-vocoder` | Gratis y con zip público de macOS | Pasar a directo en macOS (§8.2) |
| `dexed` | La release trae `Dexed-1.1.0-win.zip` (12,3 MB) | Añadir artefacto de Windows |
| `loudmax` | 1.47 del 2026-06-23; Mac AU/VST2/VST3 universal notarizado; Windows VST2/VST3; alojado en Dropbox y PlugInGuru; "freeware and not commercial in any mean", distribuir o modificar requiere permiso | Sin cambio (enlace) |
| `xfer-ott` | 1.37; macOS VST/AU/AAX (VST3 solo en el historial), Windows VST/AAX; ARM nativo desde 1.31; sin CLAP | Sin cambio; construir `fx.multiband` propio |
| `audiodamage-evil-otto` | La portada dice que todos sus plugins salen en VST3, AU, AAX y CLAP; la página de gratuitos lo describe como "Free OTT compressor" de tres bandas con compresión ascendente y descendente, sin formatos, cuenta ni lista de correo | Revisar `requires_account` y `formats` en `/pages/evil-otto` |
| `waves-free-pack` | Siete plugins (+2 StudioVerse); regalo Black Friday el 2026-11-27 | Actualizar la nota |
| `valhalla-*` | Siguen gratis (Supermassive 5.0.0) pero no se pueden verificar automáticamente | Sin cambio |
| `melda-mfreefxbundle` | Gratis con límites (sin guardar presets propios, sin redimensionar, recuadro rojo con logotipo) [V] | Anotar los límites |
| `kilohearts-essentials` | "Free with an account", 34 productos | Sin cambio |
| `decent-sampler` | 1.43.2; formulario con correo; VST, VST3, AAX, AU; sin CLAP | Sin cambio |
| `slate-free-collection` | Sigue gratis ("Yours free, forever"): 16 plugins de Slate Digital, Solid State Logic, Harrison y sonible; VST3, AU, AAX; cuenta gratuita Complete Access y su Hub; sin iLok | Sin cambio |
| `polyverse-wider` | Sigue gratis; VST2, VST3, AU, AAX; el enlace llega por correo | Sin cambio |
| `babyaudio-freebies` | Cinco plugins gratis (Warp, Magic Switch, Magic Dice, Pitch Drift, Beat Slammer) con cuenta gratuita; la página no da formatos | Sin cambio |
| `glitchmachines-hysteresis` | 1.4.0; gratis por "carrito"; VST3 y AU; macOS 11+, Windows 11 | Sin cambio |
| `ni-komplete-start` | Sigue gratis; Native Access y Kontakt Player 7.6; unos 17 GB; las páginas leídas no dicen los formatos | Sin cambio |
| `izotope-imager`, `izotope-vinyl` | La página de gratuitos de iZotope los lista y pide cuenta; no se pudo confirmar el estado de 2026 (429) | Revisar a mano |
| `zebralette-3` | 3.0.0 (diciembre de 2025) sigue siendo la última; AU, VST3, AAX y CLAP según KVR; página oficial `u-he.com/products/freeware/zebralette3/` | Cambiar `homepage` |
| `podolski` | 1.2.3 (2021-08-10); macOS AUv2 y VST3, Windows VST3 y AAX, sin CLAP | Cambiar `homepage` a `https://u-he.com/products/podolski/` |
| `tdr-slickeq` | 1.3.9 vigente; Apple Silicon nativo desde 1.3.6 | Sin cambio |

Sin leer el EULA completo: todos salvo Waves y TAL. No verificable por web: Valhalla (403) y la lista exacta del Waves Free Plugin Pack en su propia página.

---

## 6. Recomendación por candidato

Resumen de la decisión (detalle en §4 y bloques en §8):

| Carril | Candidatos |
|---|---|
| **Instalador directo en macOS y Windows** (zip en ambos) | GATE-12, FILT-R, REEV-R, JC303, OB-Xf, CloudReverb, Wolf Shaper; TIME-12 y RipplerX tras probarlos; gratis cerrado: Glitch², TDR Molotok (si es arm64) |
| **Instalador directo solo en macOS** (Windows trae `.exe` o `.msi`) | ZL Equalizer 2, ZL Compressor, ZL Splitter, BYOD, ChowTapeModel, ChowKick, ChowMatrix, ChowCentaur; gratis cerrado: TAL-Reverb-4, TAL-Chorus-LX, TAL-Filter-2, TAL-Vocoder-2. Sin bloque hasta probarlos: QDelay, SIRIAL, TetraOp, ChowMultiTool |
| **Solo enlace** | Nuevos: Airwindows Consolidated, Dragonfly, Monique, sfizz, Cardinal. Ya existentes: Vital, Kilohearts, Valhalla, Voxengo SPAN, Decent Sampler, Splice INSTRUMENT, Waves, LoudMax |
| **Descartar** | Odin 2 (el dueño ya lo descartó), Cymatics, x42, LSP (en macOS), Geonkick, Vitalium, Helm, Wolf Spectrum, plugdata, BespokeSynth, Ildaeil, DPF-Plugins. Esperar: ZL Limiter y ZL Spectrum Equalizer |

---

## 7. Orden de trabajo sugerido

1. Pegar los bloques de §8.1 y §8.2 que ya pasaron por el host (GATE-12, FILT-R, REEV-R, JC303, OB-Xf, CloudReverb, Wolf Shaper, ZL, BYOD, ChowTapeModel, ChowKick, ChowMatrix, ChowCentaur) y ejecutar `cargo xtask plugins pin <ids>`. Contrastar cada hash con el `digest` de GitHub cuando exista.
2. Probar TIME-12 y RipplerX con el arnés `real_plugins` (carga, editor abrir y cerrar, estado) antes de fijar su hash.
3. Sustituir los bloques `link` de TAL y de Glitch² por los de §8.3 y §8.4, y añadir TDR Molotok solo si `lipo -archs` muestra `arm64`.
4. Retirar `obxd` (lo sustituye `ob-xf`) y `cymatics-free`; aplicar los cambios de §8.6.
5. Cada plugin GPL/AGPL nuevo: anotar su aviso en `NOTICE` si el catálogo lo exige y mantener la descarga a petición del usuario.
6. Ejecutar `cargo xtask plugins verify` en CI y una instalación de prueba por plugin nuevo en macOS y Windows.

---

## 8. Bloques `[[plugin]]` listos para pegar

Mismo esquema que las entradas de `content/plugins/manifest.toml`. No llevan `sha256` ni `size`: los fija `cargo xtask plugins pin <id>`, que descarga cada artefacto una vez en la máquina del mantenedor. Hasta entonces, una entrada `direct` sin hash se comporta como enlace guiado.

Comprobaciones hechas el 2026-11-21 [V]:

- Los 28 bloques, unidos al manifest actual (68 entradas: 32 directas y 36 de enlace), pasan un validador en Python que replica `Catalog::validate()` (ids, categorías, licencias permitidas, formatos, hosts del fabricante, pareja `sha256`/`size`, arquitectura y contenedores). No se ejecutó cargo: al pegarlos conviene correr `cargo test -p aitrack-library`.
- Cada URL sale de `browser_download_url` en la API de GitHub (o de la página del fabricante en TAL, TDR e illformed) y respondió 200 a una petición HEAD con el mismo tamaño que declara la API o el servidor. No se descargó ningún fichero.
- Los assets recientes de GitHub traen un `digest` sha256 (ZL, tiagolr, JC303, OB-Xf, CloudReverb, Dexed). El de Dexed 1.1.0 para macOS (`f95b5d4a...300ce`) es idéntico al hash ya fijado en el manifest, así que sirve para contrastar el resultado de `pin`.
- Las líneas `# team_id = ...` salen de la copia instalada del dueño (informe 16 §1). Activarlas solo tras comprobar el bundle descargado con `codesign -dv --verbose=4`; si no coincide, el instalador rechaza el plugin (falla cerrado).
- Todos los bloques de GitHub usan `allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]`, como Surge XT y Dexed.
- Las categorías `modulation`, `glitch`, `synth` y `utility` son las más cercanas de `CATEGORIES`; no hay una para puertas rítmicas ni para instrumentos de percusión.

### 8.1 Open source: descarga directa en macOS y Windows (zip en ambos)

Nuevas entradas. En macOS los zip de tiagolr y de CloudReverb vienen sin firmar (lo dice su README): el instalador los firma ad hoc. TIME-12 y RipplerX aún no se han probado en el host; el resto sí [H].

```toml
[[plugin]]
id = "gate12"
name = "GATE-12"
vendor = "tiagolr"
category = "modulation"
version = "1.3.3"
tags = ["trance gate", "volume shaper", "rhythm", "open source"]
homepage = "https://github.com/tiagolr/gate12"
license = "GPL-3.0"
license_url = "https://github.com/tiagolr/gate12/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/tiagolr/gate12."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["GATE-12"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Gate and volume shaper with a drawn envelope: trance gates, pumping and rhythmic ducking."
notes = "[V] Asset URLs read through the GitHub API. The macOS build is unsigned (the installer signs it ad hoc). [H] Loads and opens its editor in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/tiagolr/gate12/releases/download/v1.3.3/gate12-macos-v1.3.3.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/tiagolr/gate12/releases/download/v1.3.3/gate12-win-v1.3.3.zip"

[[plugin]]
id = "filtr"
name = "FILT-R"
vendor = "tiagolr"
category = "modulation"
version = "1.3.0"
tags = ["filter", "envelope", "rhythm", "open source"]
homepage = "https://github.com/tiagolr/filtr"
license = "AGPL-3.0"
license_url = "https://github.com/tiagolr/filtr/blob/master/LICENSE"
license_notice = "Open source (AGPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/tiagolr/filtr."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["FILT-R"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Filter modulator with a drawn envelope: six filters for rhythmic movement and filter pumping."
notes = "[V] Asset URLs read through the GitHub API. The macOS build is unsigned (the installer signs it ad hoc). [H] Loads and opens its editor in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/tiagolr/filtr/releases/download/v1.3.0/filtr-macos-v1.3.0.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/tiagolr/filtr/releases/download/v1.3.0/filtr-win-v1.3.0.zip"

[[plugin]]
id = "reevr"
name = "REEV-R"
vendor = "tiagolr"
category = "reverb"
version = "1.4.0"
tags = ["convolution", "modulation", "open source"]
homepage = "https://github.com/tiagolr/reevr"
license = "GPL-3.0"
license_url = "https://github.com/tiagolr/reevr/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/tiagolr/reevr."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["REEV-R"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Convolution reverb with modulation before and after the reverb."
notes = "[V] Only the default impulse response is included (README): load your own. The macOS build is unsigned (the installer signs it ad hoc). [H] Loads and opens its editor in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/tiagolr/reevr/releases/download/v1.4.0/reevr-macos-v1.4.0.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/tiagolr/reevr/releases/download/v1.4.0/reevr-win-v1.4.0.zip"

[[plugin]]
id = "time12"
name = "TIME-12"
vendor = "tiagolr"
category = "glitch"
version = "1.2.3"
tags = ["stutter", "tape stop", "glitch", "open source"]
homepage = "https://github.com/tiagolr/time12"
license = "GPL-3.0"
license_url = "https://github.com/tiagolr/time12/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/tiagolr/time12."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["TIME-12"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Delay modulator with a drawn envelope: stutter, tape stop, glitch, scratch, reverse and pitch effects."
notes = "[V] Asset URLs read through the GitHub API. Not yet tried in the owner's host: same author and engine as GATE-12 and FILT-R, which load cleanly. Test load, editor open and close before pinning."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/tiagolr/time12/releases/download/v1.2.3/time12-macos-v1.2.3.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/tiagolr/time12/releases/download/v1.2.3/time12-win-v1.2.3.zip"

[[plugin]]
id = "ripplerx"
name = "RipplerX"
vendor = "tiagolr"
category = "synth"
version = "1.5.19"
tags = ["physical modelling", "percussion", "open source"]
homepage = "https://github.com/tiagolr/ripplerx"
license = "GPL-3.0"
license_url = "https://github.com/tiagolr/ripplerx/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/tiagolr/ripplerx."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["RipplerX"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Physically modelled synthesizer: resonators excited by a mallet or noise, for plucks, bells and percussion."
notes = "[V] Asset URLs read through the GitHub API. Not yet tried in the owner's host: test load, editor open and close before pinning."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/tiagolr/ripplerx/releases/download/v1.5.19/ripplerx-macos-v1.5.19.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/tiagolr/ripplerx/releases/download/v1.5.19/ripplerx-win-v1.5.19.zip"

[[plugin]]
id = "jc303"
name = "JC303"
vendor = "midilab"
category = "synth"
version = "0.14.0"
tags = ["303", "acid", "bass", "open source"]
homepage = "https://github.com/midilab/jc303"
license = "GPL-3.0"
license_url = "https://github.com/midilab/jc303/blob/main/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/midilab/jc303."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["JC303"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Free TB-303 style bass synthesizer with a step sequencer: acid lines."
notes = "[V] The project is GPL-3.0; some DSP files are MIT or AGPL-3.0-or-later (README). The macOS zip is universal. [H] VST3 and CLAP load in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/midilab/jc303/releases/download/v0.14.0/jc303-0.14.0-macos_universal-plugins.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/midilab/jc303/releases/download/v0.14.0/jc303-0.14.0-windows_x64-plugins.zip"

[[plugin]]
id = "ob-xf"
name = "OB-Xf"
vendor = "Surge Synth Team"
category = "synth"
version = "1.0.3"
tags = ["oberheim", "analogue", "open source"]
homepage = "https://github.com/surge-synthesizer/OB-Xf"
license = "GPL-3.0"
license_url = "https://github.com/surge-synthesizer/OB-Xf/blob/main/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/surge-synthesizer/OB-Xf."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["OB-Xf"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Open source continuation of the OB-Xd, an Oberheim OB-X style synthesizer."
notes = "[V] Replaces the obxd entry: OB-Xd 2 moved into the maker's Retromulator in 2026-11. [H] VST3 and CLAP load in the owner's host; the owner's copy is signed by Team 547U7G7MZC (set team_id after `codesign -dv`)."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/surge-synthesizer/OB-Xf/releases/download/v1.0.3/ob-xf-macOS-v1.0.3.dmg"
container = "dmg"
# team_id = "547U7G7MZC"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/surge-synthesizer/OB-Xf/releases/download/v1.0.3/ob-xf-Windows-v1.0.3.zip"

[[plugin]]
id = "cloudreverb"
name = "CloudReverb"
vendor = "xunil-cloud"
category = "reverb"
version = "0.5"
tags = ["algorithmic", "cloudseed", "open source"]
homepage = "https://github.com/xunil-cloud/CloudReverb"
license = "GPL-3.0"
license_url = "https://github.com/xunil-cloud/CloudReverb/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/xunil-cloud/CloudReverb."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["CloudReverb"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Algorithmic reverb based on CloudSeed, rewritten with JUCE."
notes = "[V] Young project (0.x). The macOS build is unsigned (the installer signs it ad hoc). [H] Loads and opens its editor in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/xunil-cloud/CloudReverb/releases/download/v0.5/CloudReverb-v0.5-macOS.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/xunil-cloud/CloudReverb/releases/download/v0.5/CloudReverb-v0.5-Windows-x86_64.zip"

[[plugin]]
id = "wolf-shaper"
name = "Wolf Shaper"
vendor = "Wolf Plugins"
category = "saturation"
version = "1.0.2"
tags = ["waveshaper", "distortion", "open source"]
homepage = "https://wolf-plugins.github.io/wolf-shaper/"
license = "GPL-3.0"
license_url = "https://github.com/wolf-plugins/wolf-shaper/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/wolf-plugins/wolf-shaper."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["wolf-shaper"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Waveshaper with a spline graph editor: distortion, wave folding and saturation."
notes = "[V] DPF plugin: host it in the sandbox (informe 16 P6). The Windows asset name has a plus sign, written %2B in the URL. [H] VST3 and CLAP load in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/wolf-plugins/wolf-shaper/releases/download/v1.0.2/wolf-shaper-v1.0.2-macos-universal.zip"
[[plugin.artifact]]
os = "windows"
url = "https://github.com/wolf-plugins/wolf-shaper/releases/download/v1.0.2/wolf-shaper-v1.0.2%2B20230515144200-windows-x64.zip"
```

### 8.2 Open source: descarga directa solo en macOS (en Windows solo hay `.exe` o `.msi`)

La app no ejecuta instaladores, así que en Windows la tarjeta lleva al usuario a `download_page`. Los dmg de ZL son uno por arquitectura. ZL y FILT-R son AGPL; el resto GPL o BSD: descarga a petición del usuario, nunca empaquetada.

```toml
[[plugin]]
id = "zl-equalizer-2"
name = "ZL Equalizer 2"
vendor = "ZL Audio"
category = "eq"
version = "1.4.2"
tags = ["dynamic eq", "analyser", "open source"]
homepage = "https://zl-audio.github.io/plugins/zlequalizer2/"
download_page = "https://github.com/ZL-Audio/ZLEqualizer/releases/latest"
license = "AGPL-3.0"
license_url = "https://github.com/ZL-Audio/ZLEqualizer/blob/main/LICENSE.md"
license_notice = "Open source (AGPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/ZL-Audio/ZLEqualizer."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["ZL Equalizer 2"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Open source equaliser with dynamic bands and a spectrum analyser."
notes = "[V] The macOS disk images are per architecture (arm64 and x86_64), not universal. Windows ships only an .msi installer: guided link. [H] VST3 loads and opens its editor in the owner's host (arm64)."
[[plugin.artifact]]
os = "macos"
arch = "arm64"
url = "https://github.com/ZL-Audio/ZLEqualizer/releases/download/1.4.2/ZL.Equalizer.2-1.4.2-macOS-arm64.dmg"
container = "dmg"
[[plugin.artifact]]
os = "macos"
arch = "x86_64"
url = "https://github.com/ZL-Audio/ZLEqualizer/releases/download/1.4.2/ZL.Equalizer.2-1.4.2-macOS-x86-64.dmg"
container = "dmg"

[[plugin]]
id = "zl-compressor"
name = "ZL Compressor"
vendor = "ZL Audio"
category = "dynamics"
version = "0.6.1"
tags = ["compressor", "bus", "open source"]
homepage = "https://zl-audio.github.io/plugins/zlcompressor/"
download_page = "https://github.com/ZL-Audio/ZLCompressor/releases/latest"
license = "AGPL-3.0"
license_url = "https://github.com/ZL-Audio/ZLCompressor/blob/main/LICENSE.md"
license_notice = "Open source (AGPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/ZL-Audio/ZLCompressor."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["ZL Compressor"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Open source compressor and dynamic range processor."
notes = "[V] The macOS disk images are per architecture. Windows ships only an .msi installer: guided link. [H] VST3 loads in the owner's host; its external sidechain input is not routed by the host yet (informe 16 P4)."
[[plugin.artifact]]
os = "macos"
arch = "arm64"
url = "https://github.com/ZL-Audio/ZLCompressor/releases/download/0.6.1/ZL.Compressor-0.6.1-macOS-arm64.dmg"
container = "dmg"
[[plugin.artifact]]
os = "macos"
arch = "x86_64"
url = "https://github.com/ZL-Audio/ZLCompressor/releases/download/0.6.1/ZL.Compressor-0.6.1-macOS-x86-64.dmg"
container = "dmg"

[[plugin]]
id = "zl-splitter"
name = "ZL Splitter"
vendor = "ZL Audio"
category = "utility"
version = "0.4.0"
tags = ["splitter", "open source"]
homepage = "https://zl-audio.github.io/plugins/zlsplitter/"
download_page = "https://github.com/ZL-Audio/ZLSplitter/releases/latest"
license = "AGPL-3.0"
license_url = "https://github.com/ZL-Audio/ZLSplitter/blob/main/LICENSE.md"
license_notice = "Open source (AGPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/ZL-Audio/ZLSplitter."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["ZL Splitter"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Open source splitter plugin: divides the signal into parts that are processed separately."
notes = "[V] The macOS disk images are per architecture. Windows ships only an .msi installer: guided link. [H] VST3 loads in the owner's host."
[[plugin.artifact]]
os = "macos"
arch = "arm64"
url = "https://github.com/ZL-Audio/ZLSplitter/releases/download/0.4.0/ZL.Splitter-0.4.0-macOS-arm64.dmg"
container = "dmg"
[[plugin.artifact]]
os = "macos"
arch = "x86_64"
url = "https://github.com/ZL-Audio/ZLSplitter/releases/download/0.4.0/ZL.Splitter-0.4.0-macOS-x86-64.dmg"
container = "dmg"

[[plugin]]
id = "byod"
name = "BYOD"
vendor = "Chowdhury DSP"
category = "saturation"
version = "1.3.0"
tags = ["distortion", "modular", "open source"]
homepage = "https://github.com/Chowdhury-DSP/BYOD"
download_page = "https://github.com/Chowdhury-DSP/BYOD/releases/latest"
license = "GPL-3.0"
license_url = "https://github.com/Chowdhury-DSP/BYOD/blob/main/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/Chowdhury-DSP/BYOD."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["BYOD"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Build-your-own distortion: a modular chain of distortion, filter and modulation modules."
notes = "[V] Windows ships only an .exe installer: guided link. [H] VST3 and CLAP load in the owner's host (about 510 parameters); the owner's copy is signed by Team 46MK76T24B (set team_id after `codesign -dv`)."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/Chowdhury-DSP/BYOD/releases/download/v1.3.0/BYOD-Mac-1.3.0.dmg"
container = "dmg"
# team_id = "46MK76T24B"

[[plugin]]
id = "chowtapemodel"
name = "ChowTapeModel"
vendor = "Chowdhury DSP"
category = "saturation"
version = "2.11.4"
tags = ["tape", "saturation", "open source"]
homepage = "https://github.com/jatinchowdhury18/AnalogTapeModel"
download_page = "https://github.com/jatinchowdhury18/AnalogTapeModel/releases/latest"
license = "GPL-3.0"
license_url = "https://github.com/jatinchowdhury18/AnalogTapeModel/blob/master/LICENSE"
license_notice = "Open source (GPL-3.0). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/jatinchowdhury18/AnalogTapeModel."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["CHOWTapeModel"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Analog tape emulation: saturation, wow and flutter, and head bump."
notes = "[V] Windows ships only an .exe installer: guided link. [H] VST3 and CLAP load in the owner's host; it reports a latency of 37 to 39 samples."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/jatinchowdhury18/AnalogTapeModel/releases/download/v2.11.4/ChowTapeModel-Mac-2.11.4.dmg"
container = "dmg"
# team_id = "46MK76T24B"

[[plugin]]
id = "chowkick"
name = "ChowKick"
vendor = "Chowdhury DSP"
category = "synth"
version = "1.2.0"
tags = ["kick", "808", "drum synth", "open source"]
homepage = "https://github.com/Chowdhury-DSP/ChowKick"
download_page = "https://github.com/Chowdhury-DSP/ChowKick/releases/latest"
license = "BSD-3-Clause"
license_url = "https://github.com/Chowdhury-DSP/ChowKick/blob/main/LICENSE"
license_notice = "Open source (BSD-3-Clause). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/Chowdhury-DSP/ChowKick."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["ChowKick"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Kick drum synthesizer based on old-school drum machine circuits."
notes = "[V] Windows ships only an .exe installer: guided link. [H] VST3 and CLAP load in the owner's host; the output can peak above 1.0 (the plugin's own gain)."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/Chowdhury-DSP/ChowKick/releases/download/v1.2.0/ChowKick-Mac-1.2.0.dmg"
container = "dmg"
# team_id = "46MK76T24B"

[[plugin]]
id = "chowmatrix"
name = "ChowMatrix"
vendor = "Chowdhury DSP"
category = "delay"
version = "1.3.0"
tags = ["delay", "matrix", "open source"]
homepage = "https://github.com/Chowdhury-DSP/ChowMatrix"
download_page = "https://github.com/Chowdhury-DSP/ChowMatrix/releases/latest"
license = "BSD-3-Clause"
license_url = "https://github.com/Chowdhury-DSP/ChowMatrix/blob/main/LICENSE"
license_notice = "Open source (BSD-3-Clause). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/Chowdhury-DSP/ChowMatrix."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["ChowMatrix"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Matrix delay effect: a network of delay lines you wire together."
notes = "[V] Windows ships only an .exe installer: guided link. [H] VST3 loads in the owner's host (about 670 parameters)."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/Chowdhury-DSP/ChowMatrix/releases/download/v1.3.0/ChowMatrix-Mac-1.3.0.dmg"
container = "dmg"
# team_id = "46MK76T24B"

[[plugin]]
id = "chowcentaur"
name = "ChowCentaur"
vendor = "Chowdhury DSP"
category = "saturation"
version = "1.4.0"
tags = ["overdrive", "open source"]
homepage = "https://github.com/jatinchowdhury18/KlonCentaur"
download_page = "https://github.com/jatinchowdhury18/KlonCentaur/releases/latest"
license = "BSD-3-Clause"
license_url = "https://github.com/jatinchowdhury18/KlonCentaur/blob/master/LICENSE"
license_notice = "Open source (BSD-3-Clause). AI-Track downloads it from the project's GitHub release only when you ask; it is a separate program, not part of AI-Track. Source: https://github.com/jatinchowdhury18/KlonCentaur."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["ChowCentaur"]
allowed_hosts = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
description = "Overdrive pedal model."
notes = "[V] Windows ships only an .exe installer: guided link. No changes since 2021. [H] VST3 loads in the owner's host."
[[plugin.artifact]]
os = "macos"
url = "https://github.com/jatinchowdhury18/KlonCentaur/releases/download/v1.4.0/ChowCentaur-Mac-1.4.0.dmg"
container = "dmg"
# team_id = "46MK76T24B"
```

### 8.3 Gratis cerrado: TAL pasa de enlace a descarga directa en macOS (sustituye a los cuatro bloques actuales; mismo `id`)

La página de productos de TAL los lista como gratuitos y sus cláusulas 7 y 11 prohíben redistribuir y hacer ingeniería inversa, así que solo se descarga de tal-software.com. El zip de macOS trae el instalador del fabricante, que se expande y nunca se ejecuta (igual que `tal-noisemaker`). Borrar antes los bloques `link` de los mismos `id`.

```toml
[[plugin]]
id = "tal-reverb-4"
name = "TAL-Reverb-4"
vendor = "TAL Software"
category = "reverb"
version = "4.0.4"
tags = ["plate"]
homepage = "https://tal-software.com/products/tal-reverb-4"
license = "Proprietary-Freeware"
license_url = "https://tal-software.com/disclaimer"
license_notice = "Free to use. AI-Track downloads it from tal-software.com, the maker's own server, only when you ask; it does not redistribute it. Read the maker's terms at https://tal-software.com/disclaimer."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["TAL-Reverb-4"]
allowed_hosts = ["tal-software.com"]
description = "Plate reverb."
notes = "[V] Listed under FREE Plug-Ins on the maker's product page. The macOS zip holds the maker's installer package, which is expanded, never run. The Windows download is an installer program, so Windows is a guided link."
[[plugin.artifact]]
os = "macos"
url = "https://tal-software.com/downloads/plugins/TAL-Reverb-4-installer_macos.zip"

[[plugin]]
id = "tal-chorus-lx"
name = "TAL-Chorus-LX"
vendor = "TAL Software"
category = "modulation"
version = "1.6.3"
tags = ["chorus", "juno"]
homepage = "https://tal-software.com/products/tal-chorus-lx"
license = "Proprietary-Freeware"
license_url = "https://tal-software.com/disclaimer"
license_notice = "Free to use. AI-Track downloads it from tal-software.com, the maker's own server, only when you ask; it does not redistribute it. Read the maker's terms at https://tal-software.com/disclaimer."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["TAL-Chorus-LX"]
allowed_hosts = ["tal-software.com"]
description = "Juno-60 style chorus."
notes = "[V] Listed under FREE Plug-Ins on the maker's product page. The macOS zip holds the maker's installer package, which is expanded, never run. The Windows download is an installer program, so Windows is a guided link."
[[plugin.artifact]]
os = "macos"
url = "https://tal-software.com/downloads/plugins/TAL-Chorus-LX-installer_macos.zip"

[[plugin]]
id = "tal-filter-2"
name = "TAL-Filter-2"
vendor = "TAL Software"
category = "modulation"
version = "3.1.2"
tags = ["filter", "trance gate"]
homepage = "https://tal-software.com/products/tal-filter"
license = "Proprietary-Freeware"
license_url = "https://tal-software.com/disclaimer"
license_notice = "Free to use. AI-Track downloads it from tal-software.com, the maker's own server, only when you ask; it does not redistribute it. Read the maker's terms at https://tal-software.com/disclaimer."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["TAL-Filter-2"]
allowed_hosts = ["tal-software.com"]
description = "Tempo-synced filter and trance gate."
notes = "[V] Listed under FREE Plug-Ins on the maker's product page. The macOS zip holds the maker's installer package, which is expanded, never run. The Windows download is an installer program, so Windows is a guided link."
[[plugin.artifact]]
os = "macos"
url = "https://tal-software.com/downloads/plugins/TAL-Filter-2-installer_macos.zip"

[[plugin]]
id = "tal-vocoder"
name = "TAL-Vocoder-2"
vendor = "TAL Software"
category = "vocal"
version = "3.0.4"
tags = ["vocoder"]
homepage = "https://tal-software.com/products/tal-vocoder"
license = "Proprietary-Freeware"
license_url = "https://tal-software.com/disclaimer"
license_notice = "Free to use. AI-Track downloads it from tal-software.com, the maker's own server, only when you ask; it does not redistribute it. Read the maker's terms at https://tal-software.com/disclaimer."
lane = "direct"
formats = ["vst3", "clap"]
bundle_ids = ["TAL-Vocoder-2"]
allowed_hosts = ["tal-software.com"]
description = "Vocoder."
notes = "[V] Listed under FREE Plug-Ins on the maker's product page (as TAL-Vocoder). The macOS zip holds the maker's installer package, which is expanded, never run. The Windows download is an installer program, so Windows is a guided link."
[[plugin.artifact]]
os = "macos"
url = "https://tal-software.com/downloads/plugins/TAL-Vocoder-2-installer_macos.zip"
```

### 8.4 Gratis cerrado: TDR Molotok (nuevo) y Glitch² (sustituye al bloque `link`)

Molotok: comprobar con `lipo -archs` que el bundle de macOS trae `arm64` antes de fijar el hash; si es solo x86_64, no añadirlo. Glitch²: el zip de macOS trae VST2, VST3 y AU y solo se instala el VST3; la gratuidad no se declara permanente.

```toml
[[plugin]]
id = "tdr-molotok"
name = "TDR Molotok"
vendor = "Tokyo Dawn Labs"
category = "dynamics"
version = "1.0.7"
tags = ["compressor"]
homepage = "https://www.tokyodawn.net/tdr-molotok/"
license = "Proprietary-Freeware"
license_notice = "Free to use. AI-Track downloads it from www.tokyodawn.net, the maker's own server, only when you ask; it does not redistribute it. Read the maker's terms at https://www.tokyodawn.net/tdr-molotok/."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["TDR Molotok"]
allowed_hosts = ["www.tokyodawn.net"]
description = "Compressor from Tokyo Dawn Labs."
notes = "[V] The page offers it as a free download, with VST3 for 64-bit Mac and Windows. Apple Silicon is not stated for the VST3 (the changelog only names AAX): check with lipo before pinning. The site terms forbid redistribution, so only a download from tokyodawn.net."
[[plugin.artifact]]
os = "macos"
url = "https://www.tokyodawn.net/labs/Molotok/1.0.7/TDR%20Molotok.zip"
[[plugin.artifact]]
os = "windows"
url = "https://www.tokyodawn.net/labs/Molotok/1.0.7/TDR%20Molotok%20(no%20installer).zip"

[[plugin]]
id = "illformed-glitch2"
name = "Glitch 2"
vendor = "illformed"
category = "glitch"
version = "2.1.5"
tags = ["stutter", "glitch"]
homepage = "https://illformed.com/glitch/"
license = "Proprietary-Freeware"
license_notice = "Free to use. AI-Track downloads it from illformed.com, the maker's own server, only when you ask; it does not redistribute it. Read the maker's terms at https://illformed.com/glitch/."
lane = "direct"
formats = ["vst3"]
bundle_ids = ["Glitch2"]
allowed_hosts = ["illformed.com", "www.illformed.com"]
description = "Glitch and stutter multi-effect, free since version 2.1.5."
notes = "[V] The page says it is completely free (in memory of Kieran Foster) and offers one zip per platform with no account. No EULA and no statement that the licence is permanent; all rights reserved, so only a download from illformed.com. The zips hold VST2, VST3 and AU builds; only the VST3 is installed."
[[plugin.artifact]]
os = "macos"
url = "https://illformed.com/downloads/Glitch_2_1_5_Mac_Free.zip"
[[plugin.artifact]]
os = "windows"
url = "https://illformed.com/downloads/Glitch_2_1_5_Windows_Free.zip"
```

### 8.5 Solo enlace (nuevas entradas)

Nombres rodantes (Airwindows, Monique), instalador `.exe` (sfizz en Windows), el instalador no puede excluir bundles (Dragonfly, ver §9) o tamaño de casi 1 GB (Cardinal). Se reconocen una vez instalados.

```toml
[[plugin]]
id = "airwindows-consolidated"
name = "Airwindows Consolidated"
vendor = "Airwindows / Paul Walker"
category = "multi"
tags = ["de-esser", "tape", "console", "about 500 effects", "open source"]
homepage = "https://github.com/baconpaul/airwin2rack"
download_page = "https://github.com/baconpaul/airwin2rack/releases/tag/DAWPlugin"
license = "GPL-3.0"
license_url = "https://github.com/baconpaul/airwin2rack/blob/main/LICENSE.md"
lane = "link"
account_hint = "Free, no account: download it from the project's release page."
formats = ["vst3", "clap"]
bundle_ids = ["Airwindows Consolidated"]
description = "About 500 Airwindows effects in one plugin: de-esser, tape, console, saturation, reverb."
notes = "[V] The Airwindows code is MIT; the DAW build links JUCE and the VST3 SDK, so the binary is GPL-3.0 (README). The release tag is rewritten every week and its file names carry a date and a hash, so it cannot be pinned. [H] VST3 and CLAP load in the owner's host (one effect selector)."

[[plugin]]
id = "dragonfly-reverb"
name = "Dragonfly Reverb (Hall, Room, Plate)"
vendor = "Michael Willis"
category = "reverb"
version = "3.2.10"
tags = ["hall", "room", "plate", "open source"]
homepage = "https://michaelwillis.github.io/dragonfly-reverb"
download_page = "https://github.com/michaelwillis/dragonfly-reverb/releases/tag/3.2.10"
license = "GPL-3.0"
license_url = "https://github.com/michaelwillis/dragonfly-reverb/blob/master/LICENSE"
lane = "link"
account_hint = "Free, no account: download it from the project's release page."
formats = ["vst3", "clap"]
bundle_ids = ["DragonflyHallReverb", "DragonflyRoomReverb", "DragonflyPlateReverb"]
description = "Three good open source reverbs: hall, room and plate."
notes = "[V] The package also holds Dragonfly Early Reflections, which hangs in the owner's host, and the installer copies every bundle it finds: link until it can exclude bundles. DPF plugins share Objective-C class names when loaded together, so host them in the sandbox (informe 16 P6). The macOS universal disk image is 25.0 MB and the Windows zip 22.4 MB."

[[plugin]]
id = "monique-monosynth"
name = "Monique Monosynth"
vendor = "Surge Synth Team"
category = "synth"
tags = ["monosynth", "open source"]
homepage = "https://github.com/surge-synthesizer/monique-monosynth"
download_page = "https://github.com/surge-synthesizer/monique-monosynth/releases/tag/Nightly"
license = "GPL-3.0"
license_url = "https://github.com/surge-synthesizer/monique-monosynth/blob/main/LICENSE"
lane = "link"
account_hint = "Free, no account: download it from the project's release page."
formats = ["vst3", "clap"]
bundle_ids = ["Monique"]
description = "Open source monophonic synthesizer."
notes = "[V] Only a rolling Nightly release exists (build of 2024-08-07, no commits since 2024-07). The code is dual licensed GPL-3.0 or MIT; the binaries use the GPL-3.0 build of JUCE (README). [H] VST3 and CLAP load, but the first prepare gives a very low peak (informe 16 P8)."

[[plugin]]
id = "sfizz"
name = "sfizz"
vendor = "SFZ Tools"
category = "sampler"
version = "1.2.3"
tags = ["sfz", "open source"]
homepage = "https://sfz.tools/sfizz/"
download_page = "https://sfz.tools/sfizz/downloads"
license = "BSD-2-Clause"
license_url = "https://github.com/sfztools/sfizz-ui/blob/develop/LICENSE"
lane = "link"
account_hint = "Free, no account: download it from the project's page."
formats = ["vst3"]
bundle_ids = ["sfizz"]
description = "SFZ sampler player."
notes = "[V] The repositories are archived (last release 2024-01-14). The macOS installer is a 11.9 MB pkg and the Windows one an .exe. AI-Track already has its own SFZ instrument. [H] VST3 loads in the owner's host (it plays a default sine without an SFZ)."

[[plugin]]
id = "cardinal"
name = "Cardinal"
vendor = "DISTRHO"
category = "synth"
version = "26.02"
tags = ["modular", "open source"]
homepage = "https://cardinal.kx.studio/"
download_page = "https://github.com/DISTRHO/Cardinal/releases/tag/26.02"
license = "GPL-3.0"
license_url = "https://github.com/DISTRHO/Cardinal/blob/main/LICENSE"
lane = "link"
account_hint = "Free, no account: download it from the project's release page."
formats = ["vst3", "clap"]
bundle_ids = ["Cardinal"]
description = "Modular synthesizer and effects rack."
notes = "[V] The macOS package is a 974 MB pkg and the Windows installer 369 MB, so it stays a link. Part of the bundled module art has its own licences. Not tried in the owner's host."
```

### 8.6 Cambios en entradas existentes

Dexed 1.1.0 ya publica un zip para Windows (12,3 MB, HEAD 200). Añadir al final de su bloque; el contenido del zip no se inspeccionó, así que conviene fijarlo y probarlo:

```toml
[[plugin.artifact]]
os = "windows"
url = "https://github.com/asb2m10/dexed/releases/download/v1.1.0/Dexed-1.1.0-win.zip"
```

Otros cambios, en el mismo orden que la tabla de §5.2:

| Entrada | Cambio |
|---|---|
| `obxd` | Borrar: lo sustituye `ob-xf` |
| `cymatics-free` | Borrar (o dejar sin promesa de formatos) |
| `klanghelm-mjuc-jr` | `homepage = "https://klanghelm.com/contents/products/MJUC"` |
| `wavesfactory-free` | `homepage = "https://www.wavesfactory.com/free-vst-plugins/"` |
| `softube-saturation-knob` | `homepage = "https://www.softube.com/plug-ins/saturation-knob"` |
| `slate-free-collection` | `homepage = "https://slatedigital.com/free-collection/"` |
| `analogobsession` | `homepage = "https://analogobsession.com/"` y nota "la página solo lista macOS hasta 13.x" |
| `zebralette-3` | `homepage = "https://u-he.com/products/freeware/zebralette3/"` |
| `podolski` | `homepage = "https://u-he.com/products/podolski/"` |
| `splice-instrument` | `formats = ["vst3"]`; descripción: "Gratis con cuenta Splice; sustituye a Spitfire LABS, que perdió el soporte el 2026-10-31" |
| `voxengo-span` | Nota: "En macOS solo AU, AAX y VST2: útil solo en Windows" |
| `waves-free-pack` | Nota: siete plugins (Lil Tube, IR Live Reverb, V-EQ3, V-Comp, AudioTrack, Flow Motion, GTR Solo) y un regalo de Black Friday el 2026-11-27 |


---

## 9. Huecos del instalador que estos candidatos destapan

1. `Container` no tiene `msi` (ZL en Windows), ni instalador `.exe` (ChowDSP, Dexed `.exe`, Geonkick) ni `7z` (LSP). Por eso esas plataformas quedan como enlace guiado.
2. `ALLOWED_LICENSES` no incluye `LGPL-3.0` ni `GPL-2.0`: rechazaría LSP, x42 e Ildaeil (que de todos modos se descartan).
3. `CATEGORIES` no tiene "drum" ni "instrument": los sintes y el kick usan `synth`.
4. Un tag rodante (Airwindows, Monique, nightlies de Surge) no se puede fijar: `cargo xtask plugins verify` fallaría cada semana.
5. **El instalador no puede excluir bundles de un paquete.** `install_in` instala todos los `.vst3`/`.clap` cuyo formato figura en `formats` (`crates/library/src/plugins.rs`), y `bundle_ids` solo sirve para reconocer lo ya instalado. Dragonfly trae Early Reflections, que cuelga en el host del dueño: hace falta un campo nuevo (por ejemplo `exclude_bundles`) para pasarlo a directo.
6. `verify` detecta que cambió el fichero de una URL, pero no que exista una versión nueva: convendría una comprobación semanal por la API de GitHub que compare etiquetas.
7. GitHub publica el sha256 (`digest`) de los assets recientes: `pin` podría contrastarlo con el hash que calcula y avisar si difieren.
8. Los zip de tiagolr, CloudReverb y otros builds de macOS sin firmar dependen de que `check_signature` los firme ad hoc: ya lo hace, pero conviene una prueba de instalación en Apple Silicon por cada plugin nuevo.

---

## 10. Límites de esta investigación

- No se descargó ningún binario: **el contenido de cada zip, dmg o pkg no se inspeccionó**. Qué bundles trae cada paquete (y si el dmg de ChowDSP u OB-Xf lleva un `.pkg` dentro) sale de los README, de las copias instaladas del dueño [H] y de cómo funciona `open_container`, no de abrir los ficheros. `cargo xtask plugins pin` y una instalación de prueba lo confirmarán.
- Los datos [H] son del informe 16 (2026-11-15) y de las versiones que el dueño tenía instaladas. No hay pruebas en Windows (informe 16 P18) ni en Mac Intel.
- Las páginas se leyeron con una herramienta que resume. Solo se leyeron cláusulas de Waves (EULA de septiembre de 2026) y de TAL (`/disclaimer`); el resto de EULA no se leyó (Tokyo Dawn, Klanghelm, u-he, Valhalla, Voxengo, Youlean, Kilohearts, Decent Sampler, illformed y los demás) y de Cymatics solo sus Términos de uso.
- Valhalla devuelve 403 a los clientes automáticos; iZotope y Cymatics devolvieron 429 en varias páginas; la lista del Waves Free Plugin Pack sale de la prensa (MusicRadar), no de waves.com.
- TIME-12, RipplerX, QDelay, SIRIAL, TetraOp y ChowMultiTool no se han probado en el host.
- Quedan por confirmar el Apple Silicon de TDR Molotok y la permanencia de la gratuidad de Glitch².
- `open_issues` de GitHub incluye pull requests, y las cuentas de `crash` y `hang` son una búsqueda por palabra, no un triaje.
- Esto no es asesoría legal.
