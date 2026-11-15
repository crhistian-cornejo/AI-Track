# 15 · Integración tipo Splice (panel lateral con preview a tempo y tonalidad)

Fecha: 15-nov-2026. Autor: investigación automatizada, solo lectura (no se tocó código, git ni se ejecutó `cargo fmt`).

Convención: **[V]** verificado (con URL o `ruta:línea`), **[S]** supuesto o inferencia que hay que comprobar. Las páginas web se leyeron con una herramienta que resume el texto, así que las citas son paráfrasis fieles pero **no sustituyen una revisión legal** del texto íntegro.

---

## 0. Resumen ejecutivo

1. **Splice no tiene API pública, SDK ni programa de partners abierto** [V: no aparece ninguno en su web, su blog ni su soporte; un índice de APIs de terceros lo afirma expresamente, https://apis.io/secondary-market/splice/ — fuente secundaria]. Las integraciones nativas (Studio One, Pro Tools, Ableton Live 12.3) son acuerdos bilaterales con cada fabricante de DAW.
2. **Ya existe en el repo un crate `crates/splice` que usa la API web no oficial de Splice** (GraphQL privado + descrambling de los MP3 de preview) [V: `crates/splice/src/lib.rs:1-12`, `query.rs:10`, `descramble.rs:1-8`, `NOTICE:17-24`]. Los Términos de Uso de Splice (actualizados el 2-nov-2026) prohíben herramientas automáticas que extraigan datos de las webs (§2.12(j)), eludir funciones que limitan el uso o copia de contenido (§2.12(a)) y usar los previews fuera de uso "interno y local" (§2.3.1.4(f)); el incumplimiento permite suspender la cuenta (§11.4) [V: https://splice.com/terms]. Hoy solo busca y escucha previews (no licencia ni descarga; el sign-in no funciona sin un client id que Splice no publica; el filtro de key no está conectado; no hay arrastre ni sync). **Recomendación: sustituir el lado online no oficial y complementarlo con la carpeta local de Splice; no enviarlo en builds de release** (ver §C4).
3. **Opción recomendada**: un panel lateral "Sonidos" alimentado por (a) la carpeta local de samples que el usuario ya licenció con Splice (indexada por `crates/library`), con filtros por tonalidad/BPM y **preview sincronizado a tempo y tono de la canción**, (b) fuente online con API legal (Freesound, con matices) detrás de un trait de proveedores, y (c) opcionalmente **alojar los plugins oficiales de Splice (Bridge / Sounds Plugin, VST3) en una pista**, que es la vía oficial para DAWs sin acuerdo.
4. **Lo que ya tenemos**: filtro por key y BPM (SQLite + FTS), compatibilidad armónica (relativa, Camelot ±1), tempo con ½×/2×, `Warp` con `original_bpm`/`transpose`, arrastre al arrangement con key y tempo del loop, detector de key/BPM en `crates/ai`. **Lo que falta**: el preview sincronizado (hoy suena crudo, sin estirar ni transponer), el cálculo de transposición con modo (relativas) y los semitonos mínimos, el panel lateral persistente (el sidebar fue sustituido por la paleta Cmd+K), y que el arrastre aplique transposición.

---

## Parte A · Investigación web

### A1. Cómo funciona Splice dentro de Ableton Live

| Hecho | Fuente |
|---|---|
| Llegó con **Live 12.3** (release 25-nov-2025, tras beta pública anunciada en sept-2025). Splice "integrado en el navegador de Live" | [V] https://www.ableton.com/en/release-notes/live-12/ ; https://splice.com/blog/letter-from-our-ceo-september-2025/ |
| Solo Live 12.3+, en todas las ediciones (Intro, Standard, Suite) | [V] https://support.splice.com/en/articles/12062860-faq-splice-integration-with-ableton-live-12-3 |
| Aparece como etiqueta "Splice" en la barra lateral del navegador (ocultable con "Hide from Sidebar" o "Show Splice: Off" en Library Settings) | [V] https://www.ableton.com/en/live-manual/12/working-with-the-browser/ (§4.7.3) |
| **Preview a tempo**: los loops se sincronizan al tempo del Set por defecto; el botón "1x BPM" desactiva el Timestretch y reproduce al tempo original. Los one-shots hacen preview en bucle por defecto; "1 BAR" lo desactiva | [V] manual de Live, mismo enlace |
| **Preview a tono**: los ajustes de Splice Preview permiten transponer a una tonalidad elegida y sincronizar con la escala del Set. Filtro Key con opción **"Apply Key from DAW"** (usa la raíz y escala de la barra de transporte con Scale Mode activo) | [V] manual de Live; notas de versión 12.3 |
| **Search with Sound**: arrastras un clip o haces una selección de tiempo, pulsas el widget y Splice devuelve **50** samples compatibles. Requiere cuenta | [V] manual de Live; notas 12.3 |
| Splice garantiza que el audio subido a Search with Sound solo se usa para buscar, sin entrenar IA ni compartir | [V] FAQ de Splice (12062860) |
| Cuentas: sin cuenta se puede explorar y escuchar; hace falta cuenta para añadir al Set, guardar en la librería o Search with Sound; cuenta gratuita ≈ 2.500 samples incluidos | [V] manual de Live |
| Plan "Splice x Ableton Live" 12,99 USD/mes; Creator/Creator+/Ableton: hasta **100 sonidos/día** sin créditos; planes Sounds+ y heredados usan créditos | [V] FAQ 12062860 |
| Arrastrar al Set (pista de audio/MIDI, Simpler, Sampler, Drum Rack, Impulse) **licencia y descarga** el sample automáticamente a `User Library > Samples > Splice` (configurable: User Library, Current Project o carpeta propia) y lo sincroniza con la librería de Splice (escritorio, web, móvil) | [V] https://support.splice.com/en/articles/12059504-how-to-download-splice-samples-in-ableton-live-12-3 ; https://support.splice.com/en/articles/8652631-where-do-my-downloaded-samples-presets-midi-files-go |
| Solo se descargan samples (no presets, MIDI ni "Get Pack") | [V] FAQ 12062860 |
| Live comprueba actualizaciones de Splice al arrancar; el componente de Splice se instala con un instalador propio versionado (1.1.0 en 12.4.3, 1.2.1 en 12.4.5) | [V] notas de versión de Live 12.4.x |
| ¿Exige la app de escritorio de Splice? Los documentos leídos **no lo dicen**; el instalador separado sugiere que el componente va incluido en Live | [S] |

Se describe como "exclusive In-DAW integration" de Splice para Live 12.3+ [V, FAQ 12062860].

### A2. API pública, SDK, partners y términos

- **Sin API/SDK/partner program públicos** [V: ninguna página de Splice lo menciona; la carta del CEO tampoco; índice de terceros lo confirma — fuente secundaria].
- Integraciones nativas conocidas:
  - **Studio One / Studio Pro** (PreSonus/Fender): "primera integración con un DAW"; requiere plan Splice x Studio Pro, Studio One Pro 7 o Studio Pro 8; 100 sonidos/día [V] https://support.splice.com/en/articles/9953074-all-you-need-to-know-about-using-the-splice-studio-one-integration ; https://splice.com/blog/splice-integration-studio-one-update-november-2024/
  - **Pro Tools 2025.6+** (Avid): buscar, filtrar y escuchar sincronizado a la tonalidad y tempo del proyecto, con captura de audio de la sesión [V] https://splice.com/blog/splice-integration-pro-tools/ ; nota de prensa de Avid citada en `docs/research/03-daw-workflow-ux.md:555`.
  - **Ableton Live 12.3+** (arriba).
  - Para el resto de DAWs: plugins **Splice Bridge** (VST3/AU) y el nuevo **Splice Sounds Plugin** (beta, AU y VST3; macOS 12+, Windows 10+; "funciona en cualquier DAW que acepte VST/AU"; no incluido en el plan INSTRUMENT) [V] https://support.splice.com/en/articles/12997860-faq-splice-sounds-plugin-now-in-beta ; https://support.splice.com/en/articles/12997591-getting-started-with-the-splice-sounds-plugin-now-in-beta
- **Términos de Uso** (Distributed Creation Inc., "Last Updated: November 2, 2026", https://splice.com/terms) [V vía resumen]:
  - §2.12 "Prohibited Use": (a) "disabling or circumventing features that prevent or limit use or copying of any content" y reverse engineering; (j) herramientas manuales o automáticas para "scrape" o descargar datos de las webs (robots, crawlers); (l) no vender ni compartir datos obtenidos de los Servicios; (o) no usar los Servicios como material de entrenamiento de IA; (p) no ayudar a otros a hacerlo.
  - §2.3.1.4(f): los sonidos "preview" solo se pueden usar "internally and locally". §2.3.1.4(a),(d): prohibido sublicenciar los sonidos aislados o redistribuirlos en nuevos packs.
  - §11.4: suspensión/terminación de la cuenta por incumplimiento.
  - **No hay cláusula específica sobre clientes no oficiales ni términos de API** (no existe API oficial). Ausencia de prohibición explícita ≠ permiso: §2.12 dice que los Servicios solo pueden usarse "para su propósito previsto".

### A3. Cómo guarda Splice los sonidos en disco

- **App de escritorio**: carpeta `Splice` del usuario (Mac `~/Splice`; Windows `C:\...\Splice`), organizada por pack; los items añadidos a la librería con la app abierta se descargan solos [V] https://support.splice.com/en/articles/8652631-where-do-my-downloaded-samples-presets-midi-files-go . Se puede cambiar en Preferencias ("Set New Folder") [V, fuente de 2020, posiblemente desactualizada: https://pakroses.blogspot.com/2020/04/splice-app-not-downloading-samples.html].
- La subcarpeta `sounds` que asume `aitrack_splice::default_sounds_folder()` (`~/Splice/sounds`, `crates/splice/src/lib.rs:41`) **no se pudo confirmar en la documentación oficial** [S; la ayuda oficial solo cita `~/Splice`]. Una carpeta por defecto errónea haría que "Index now" diga "No Splice folder" (`crates/app/src/views/prefs.rs:332`).
- Descargas en Ableton → `User Library/Samples/Splice`; en Pro Tools/Studio Pro → carpeta Splice; Splice Sounds Plugin v0.7.0+ → carpeta Splice por pack, contenido compartido con la app de escritorio [V, mismo artículo 8652631 y https://support.splice.com/en/articles/13007631].
- Formato: WAV (el plugin permite "Show enclosing folder → .WAV") [V parcial, artículo 13007631]; la ayuda **no documenta metadatos embebidos** (ACID/BWF) [V: no mencionados]. Convención de nombres con BPM y tonalidad en el nombre del archivo: es habitual pero **[S]**; hay que validarla con nombres reales (ver trabajos).
- **Base de datos local**: un proyecto de terceros indica que el cliente guarda un `sounds.db` (SQLite) que se obtiene con "Download logs" en los ajustes, bajo `SpliceLogs/users/default/<usuario>/` [V como afirmación de tercero: https://github.com/ebai101/splorganizer — README]. **No existe esquema publicado**; no consta qué tablas/columnas tiene BPM/key/tags [V: no encontrado]. No es una interfaz documentada ni soportada por Splice → no depender de ella.
- **Splice Bridge** (VST3/AU en una pista MIDI): conecta el DAW con la app de escritorio, que debe estar abierta; detecta el tempo del proyecto y alinea los previews a su rejilla; la **tonalidad se fija manualmente en la app** (Transpose) y las muestras con key se transponen; tempo normal/½×/2× [V] https://support.splice.com/en/articles/8652855 ; https://splice.com/blog/how-to-use-splice-bridge . Probado oficialmente en Ableton 10+, Logic 10.5+, Studio One 5+, FL Studio 20.8+, GarageBand 10.4+ [V, soporte 8652858 vía búsqueda]. Los samples arrastrados adoptan por defecto la transposición/timestretch de la app (opción desactivable) [V, artículo 13007631].
- **Splice INSTRUMENT**: plugin de sonidos con presets propios; los previews del plan INSTRUMENT no incluyen el Sounds Plugin [V, FAQ del plugin]. No se encontró documentación de qué expone a terceros [V: nada].

### A4. Alternativas con API legal

| Servicio | API pública | Auth / límites | Licencia / observaciones | Fuente |
|---|---|---|---|---|
| **Freesound** | Sí (APIv2) | API key (`token`) para búsqueda y previews; **OAuth2** para descargar el original. 60 req/min y 2.000/día (30/min y 500/día para descargas originales). Filtros `bpm`, `tonality`, `note_name`, `duration`, `license`, `loopable`; previews MP3/OGG (`preview-hq-mp3` ≈128 kbps) sin OAuth | **Uso comercial de la API "se negociará caso a caso" con UPF**; sin scraping ni copias completas de la base; claves confidenciales, una por aplicación; caché solo temporal; cada sonido lleva su licencia (CC0/CC-BY/CC-BY-NC… → guardar autor y licencia, CC-BY exige atribución) | https://freesound.org/docs/api/overview.html ; https://freesound.org/docs/api/resources_apiv2.html ; https://freesound.org/help/tos_api/ |
| **BandLab Sounds** | No encontrada | — | Licencia no exclusiva, libre de royalties para música propia, prohibido redistribuir sueltos/como pack | https://help.bandlab.com/hc/en-us/articles/360018942593-BandLab-Sounds |
| **Loopcloud (Loopmasters)** | No encontrada | — | Tiene plugin propio con sync de tempo/tono (VST/AU/AAX); contacto de partners por correo, sin documentación de API | https://help.loopmasters.com/hc/en-us/articles/27012746051740-How-to-sync-Loopcloud-with-your-DAW |
| **LANDR** | API de masterización solo con acuerdo; los endpoints de samples están tras login de partner | Partner | — | https://apitracker.io/a/landr (secundaria) |
| **Noiiz / Sounds.com** | No se encontró documentación | — | Habría que preguntar | [V: ausencia] |

Conclusión: **la única opción online con API abierta y documentada es Freesound**, con contenido heterogéneo en calidad y licencias y con condiciones comerciales a negociar. Para el resto, el camino es un acuerdo de partner.

---

## Parte B · Estado local (solo lectura)

- `/Applications/Splice.app`, `~/Splice`, `~/Library/Application Support/com.splice.Splice`, `~/Library/Application Support/Splice`, `Splice Bridge`, `~/Library/Caches/com.splice.Splice`, `~/Library/Preferences/com.splice.Splice.plist`, `~/Library/Logs/Splice`: **todos ausentes** [V, comprobados con `[ -e ]`]. No hay plugins `*Splice*` en `/Library/Audio/Plug-Ins/{VST3,Components}` ni `~/Library/Audio/Plug-Ins/VST3` [V].
- Hay una carpeta propia del usuario en Google Drive llamada "2. Splice Sounds": **vacía** (0 entradas) [V].
- Por tanto **no hay base de datos de Splice cuyo esquema listar** y no se abrió ninguna credencial ni token. Cualquier afirmación sobre `sounds.db` es de terceros [S].
- Rastros en el repo: `crates/splice` (compilado en `target/`) y `crates/app/src/views/palette/splice.rs`. El `KeychainStore` no se ejecutó ni se leyó.

---

## Parte C · Auditoría de nuestro código

### C1. `crates/library`

| Pieza | Qué hay | Evidencia |
|---|---|---|
| Esquema SQLite | `samples` con `bpm`, `bpm_source`, `key_root`, `scale_kind`, `camelot`, `key_source`, `root_note`, `kind`, `is_loop`, `category`, `genres`, `pack`, `license`, `embedding`, `cloud`, `hash`; índices por key/camelot/bpm; FTS5 `samples_fts`; tabla `packs` con `license`, `attribution` | `db.rs:82-172` |
| `Sample` | expone `scale()`, `key_label()`, `root_note`, `is_loop()` | `db.rs:240-290` |
| Filtros | `KeyFilter{scale, mode: Exact/Compatible, include_atonal}` y `BpmFilter{bpm, tolerance_pct, half_double}`; filtros por categoría, loop, camelot, pack, favoritos | `query.rs:90-134` |
| Compatibilidad armónica | `key_compat`: misma 1,0; relativa 0,9; Camelot ±1 0,8; paralela 0,5; raíz-sola en escala 0,85 | `query.rs:42-77` |
| Transposición | `transpose_to(pitch_class, target)` → −6..+5, **solo alinea la tónica, ignora el modo** | `query.rs:80-83` |
| Tempo | `tempo_compat`: compara a ½×, 1× y 2×; ±6 % graduado | `query.rs:314-330` |
| Recomendador | `recommend()` puntúa 0,5 key + 0,35 tempo; propone `transpose` solo para one-shots con `k<0.8` | `query.rs:372-440` |
| Parseo de nombres | key y BPM desde archivo/carpeta (EN+ES), con tabla de pruebas ("Kick_Am_128bpm.wav", "Loop 128 BPM.wav"…) | `parse.rs:983-1000, 1140-1160` |
| Análisis | `Analyzer` (trait), `BasicAnalyzer`; `reanalyse` detecta key/BPM desde audio y los **cachea por hash**; no lee chunks ACID/BWF (grep vacío) | `analyze.rs:81-93`, `reanalyse.rs`, `scan.rs:38` |
| Carpetas virtuales | árbol por categoría, key, Camelot, rangos de BPM, géneros, packs | `tree.rs:12-40` |
| Vigilancia | `notify` con debounce; indexa lo nuevo y marca lo borrado, también en carpetas cloud | `watch.rs:1-40` |
| Cloud | placeholders de Dropbox/OneDrive/Drive/iCloud: se indexan por nombre (key/BPM del nombre) y se descargan al reproducir o arrastrar | `cloud.rs`, `app/src/views/palette/libraries.rs:312-354` |
| Hub | manifest de packs redistribuibles (CC0/CC-BY) y enlaces | `hub.rs:1-25` |
| Splice | sólo aparece como entrada de catálogo de etiquetas | `labels.rs:80` |

### C2. App (UI, preview, arrastre)

- **No hay sidebar de navegador**: `browser.rs` quedó reducido a ayudantes; la paleta Cmd+K lo sustituyó [V: `crates/app/src/views/browser.rs:1-4`]. Existe una pestaña "Splice" opt-in en la paleta (`palette/mod.rs:84-97, 590, 843-870, 2028`; `palette/splice.rs`) y ajustes en `prefs.rs:325-430`.
- **Preview**: `real/preview.rs` es una voz propia en un **stream cpal aparte** (mismo dispositivo), decodifica en un worker, lee con interpolador a `ratio × sr_archivo / sr_stream`, límite 30 s [V: `real/preview.rs:1-60`]. API: `preview(path)` y `preview_pitched(path, ratio, reverse)` [V: `real/engine.rs:615-623`]. **No hay time-stretch ni sincronización a transporte**; `preview_pitched` solo lo usa el editor de samples (`views/sample_editor/mod.rs:309`). La paleta llama a `engine.preview(Some(path))` sin ajuste (`palette/mod.rs:827-832`).
- **Motor de warp** (`crates/dsp/src/warp.rs`): `warp_audio` offline (Beats, Tones, Texture, Complex, ComplexPro, Repitch) con caché en el motor; hay evaluadores sin asignaciones para tempo dinámico [V: `warp.rs:1-17, 634, 707, 735`]. Es la base para el preview ajustado.
- **Arrastre al arrangement**: `DraggedSample` (`workspace.rs:347`) → `add_sample_clip` (`workspace.rs:1878`): crea fuente+clip en una sola edición (un paso de undo); copia `key` y, para loops, `warp.original_bpm` y `warp.enabled = true` si hay BPM (`workspace.rs:1905-1915`). **No aplica `warp.transpose`** ni warpea one-shots; `Warp.transpose` admite ±48 st y `detune` ±50 ct [V: `model/src/project.rs:583-599`]. Los cambios van por `Command::AddSource/SetWarp` [V: `model/src/command.rs:111, 404`].
- **Tonalidad y tempo del proyecto**: `project.scale: Option<Scale>` y `project.tempo: TempoMap` [V: `palette/mod.rs:96`-uso de `ws.s.project.scale`, `model/src/project.rs:48`]. `Scale::mask()` da la máscara de 12 bits de pitch-classes [V: `model/src/scale.rs:126`].
- **Drop de ficheros externos**: solo en el oscilador del sinte y el panel IA (`ExternalPaths`); **el arrangement no los acepta** [V: grep]. Relevante si se alojan plugins de Splice (arrastre desde la ventana del plugin).
- **Plugins**: el crate `host` aloja VST3/CLAP [CLAUDE.md]; no se verificó que pase tempo/transporte a plugins de forma que Bridge lo reciba [S: `host/src/vst3/instance.rs:79` tiene `ProcessContext` del SDK; contenido no auditado].

### C3. `crates/ai`

- `detect_key(samples, sr)` (chroma HPCP + perfiles Krumhansl–Kessler y Temperley; devuelve `KeyEstimate`) y `score_keys`; `detect_bpm_with`, `bpm_from_filename` [V: `ai/src/key.rs:1-25, 133-175`, `ai/src/tempo.rs:43-54, 248-255`]. Sirven para "Search with Sound" local y para rellenar samples sin metadatos. Precisión no medida en este informe [S].

### C4. Auditoría de `crates/splice` y su UI (integración que YA existe)

Auditoría hecha leyendo código, tests y fixture; **no se llamó a la red real, no se inició sesión y no se leyó el llavero**. `cargo test -p aitrack-splice --offline`: 18 tests pasan (parseo del fixture, consulta, PKCE, descramble, loopback local, `MemoryStore`). Ninguno prueba contra el servidor real, así que **que la consulta GraphQL siga funcionando hoy es [S]**. El crate no tiene README (el aviso de "API no oficial" está en `lib.rs:1-14`, la `description` de `Cargo.toml`, `NOTICE:17-24` y `prefs.rs:367`). Es miembro del workspace y `aitrack-app` lo enlaza siempre (`Cargo.toml:11,40`, `crates/app/Cargo.toml:22`); en ejecución está **apagado por defecto** (`PaletteSettings.splice = false`, `palette/settings.rs:45-50`) y el servicio se crea al activarlo.

**C4.1 Qué hace hoy, de extremo a extremo**

| Etapa | Qué ocurre realmente | Evidencia |
|---|---|---|
| Búsqueda | POST a `https://surfaces-graphql.splice.com/graphql` (el GraphQL interno de la web de Splice) con una consulta `assetsSearch` propia: texto, orden (relevancia, popularidad, recencia, aleatorio, BPM, key…), tipo loop/one-shot, rango de BPM, key (`C`…`B` + `major/minor`), tags, paginación (50, máx. 100). Respuesta: nombre, tags, BPM, key, `chord_type`, duración (ms), archivos (preview MP3, waveform JSON), pack (nombre, cover, permalink) y resumen de tags. Hilo propio que solo atiende la consulta más reciente | `query.rs:10, 84-140`; `model.rs:70-200`; `service.rs:76-95` |
| Autenticación | Opcional. Flujo OAuth2 con PKCE y redirección loopback `127.0.0.1:<puerto>/callback`; el token va al Llavero (`dev.aitrack.splice`). **Solo funciona si el entorno define `AITRACK_SPLICE_CLIENT_ID`**: Splice no publica registro de clientes de terceros (lo dice el propio código), así que en una build normal el botón responde "no está configurado en esta build". Los endpoints por defecto (`auth.splice.com/authorize`, `/oauth/token`) y el scope son suposiciones no verificadas [S]. Si hay token, solo se envía como `Authorization: Bearer` en la búsqueda; **no se usa para nada más** (no hay licencia ni descarga) | `auth.rs:1-47`; `service.rs:154-176`; `client.rs:44-47` |
| Previsualización | Descarga el MP3 de preview (máx. 16 MB, solo https), lo **desembaraza** (descramble) en memoria, lo guarda en la caché de la app (`<config>/splice-cache/previews/<uuid>.mp3`) y lo reproduce con `engine.preview(Some(path))` (el mismo preview crudo de las muestras locales) | `client.rs:71-84`; `palette/mod.rs:757`; `palette/splice.rs:34-36` |
| "Descramble" | Splice entrega el preview ofuscado: cabecera de 28 bytes con una longitud `n` (bytes 2..10) y una clave de 18 bytes (10..28); tras la cabecera, los rangos `0..n` y `2n..3n` del audio están en XOR con la clave repetida. El código lo deshace para obtener un MP3 reproducible (con tests de ida y vuelta). Es lo que hace la propia web de Splice para reproducir el preview, replicado fuera de ella | `descramble.rs:1-30`; `NOTICE:19-23` |
| Descarga / licencia | **No existe.** El botón de descarga de cada fila y Enter/doble clic solo abren la página del sample o pack en splice.com (`cx.open_url`) con el aviso "licénciala allí y aparecerá en tu carpeta Splice". No hay llamada de licencia ni de descarga del WAV | `palette/mod.rs:966-971, 1375-1383`; `lib.rs:42-49` |
| Carpeta local | Ajuste "Splice folder" (por defecto `~/Splice/sounds`, subcarpeta **sin confirmar** [S]) + botón "Index now" que llama a `library.add_folder`; así lo licenciado vuelve como samples locales | `lib.rs:35-39`; `prefs.rs:325-335, 386-408` |
| Portadas y ondas | Descarga de cover (≤4 MB) y waveform JSON (zlib o plano) reducida a 48 barras | `client.rs:88-110`; `service.rs:97-108` |
| UI | Pestaña "Splice" (y sección en "All" cuando hay texto) dentro de la paleta Cmd+K; filas con portada, tags clicables como filtro, onda, duración, key, BPM; marca la key si coincide con la escala del proyecto (`keys_match`) | `palette/mod.rs:457-462, 582-617, 1342-1394` |

**C4.2 Qué NO funciona o no está conectado**

1. **Filtro por tonalidad del proyecto: no está conectado.** `refresh` construye los parámetros de Splice con `key = None` (`palette/mod.rs:461`), aunque existen `splice_key(Scale)` y su test (`palette/splice.rs`) y la consulta admite key y `chord_type`. La pestaña "In key" solo consulta la librería local (`mod.rs:441-452`). El BPM sí se pasa, pero como rango manual de la UI, no como tempo del proyecto; no hay tolerancia ni ½×/2× en el lado Splice.
2. **Preview sin sincronizar**: reproduce el MP3 tal cual, sin estiramiento al tempo, sin cuantización al compás y sin transposición (`palette/mod.rs:757`, `real/engine.rs:615`). El `ratio` de `preview_pitched` existe pero no se usa aquí.
3. **No se puede arrastrar un resultado de Splice al arrangement**: las filas de Splice no tienen `on_drag` (solo las locales usan `DraggedSample`, `mod.rs:1318`). Y un preview es un MP3 de calidad reducida que no debe acabar en un proyecto (§2.3.1.4(f)).
4. **Sign-in efectivo = inexistente** en builds normales (sin client id) y, con él, sin utilidad: no hay endpoint de licencia ni de descarga.
5. **Datos de demostración** en el modo dev: una fila ficticia "Splice ~/Splice/sounds, 1.284 samples" (`libraries.rs:66`).
6. **No hay panel lateral**: todo vive en la paleta modal (`views/browser.rs:1-4`).
7. **Fragilidad**: la forma de la consulta y de las URLs (`spliceproduction.s3…` en el fixture, que es sintético) se aprendió de un proyecto de terceros; si Splice cambia el esquema, la UI muestra "Splice: …" y deja reintentar (`palette/splice.rs`). No hay límite de peticiones propio más allá de "solo la consulta más reciente".

**C4.3 Riesgo legal y de términos de uso** (https://splice.com/terms, "Last Updated: November 2, 2026"; lectura vía resumen, **requiere revisión de un abogado**)

| Comportamiento del crate | Cláusula | Cita corta |
|---|---|---|
| Consultar el GraphQL interno de la web con un cliente propio, sin API ni permiso | §2.12(j) | "use any manual or automated software, devices or other processes" para extraer datos de las webs; scrapers, robots, crawlers |
| Deshacer la ofuscación de los previews | §2.12(a) | "disabling or circumventing features that prevent or limit use or copying of any content" |
| Guardar el preview en disco (aunque sea caché) y reproducirlo en un producto de terceros | §2.3.1.4(f) | los "preview" solo se pueden usar "internally and locally" |
| Reusar un client id OAuth ajeno / ingeniería inversa del flujo | §2.12(a)(ii), §2.12(c) | reverse engineering; suplantación (a evaluar) |
| Distribuirlo como software abierto | §2.12(p) | no ayudar a otros a hacer lo anterior |
| Consecuencia para el usuario | §11.4 | Splice puede suspender o terminar la cuenta |

- **Que no haya cláusula contra "clientes no oficiales" no equivale a permiso**: §2.12 limita los Servicios a su "propósito previsto" y no hay API oficial en la que apoyarse (§A2).
- El riesgo recae sobre **los usuarios** (sus cuentas) y sobre **el proyecto** (carta de cese, retirada del repo). La advertencia de Preferencias ("API web no oficial… puede dejar de funcionar", `prefs.rs:367`) describe el riesgo técnico, **no** el contractual.
- Documentos propios en contradicción: `PLAN.md:359` ("solo con las vías oficiales y el login del usuario, sin tocar credenciales"); `CLAUDE.md` no menciona el crate; `NOTICE:17-24` dice MIT para splicerr, pero la página del repositorio no mostraba archivo de licencia [V: https://github.com/Robert-K/splicerr] → verificar antes de seguir citándola.

**C4.4 Qué falta para lo que pide el dueño**

| Requisito | Estado | Falta exactamente |
|---|---|---|
| Panel lateral fijo | No existe (solo paleta modal) | Componente de panel acoplable, estado propio, lista virtualizada reutilizando las filas de la paleta, fuente seleccionable (§D2) |
| Preview sincronizado al tempo del proyecto | No existe para ninguna fuente | Render offline con `warp_audio` + arranque cuantizado al compás + lectura del transporte (§D2). Con previews de Splice es además ilegítimo (MP3 reducido, §2.3.1.4(f)); con samples locales licenciados, sí |
| Filtrado por la tonalidad de la canción | Local: sí (`KeyFilter`, `key_compat`). Splice: parámetro existente pero **sin conectar** (`mod.rs:461`) | Local: ya está. Splice: conectar `key`/modo; además la consulta solo admite una key+modo exactos (sin relativas ni Camelot) |
| Transposición a la tonalidad | No existe (`transpose_to` solo alinea tónica, ignora modo) | Módulo `fit` por máscaras (§D2), preview con transposición y `AddSource.warp.transpose` al arrastrar |
| Arrastre a la pista con warp ajustado | Local: sí en tempo (loops con `original_bpm`), no en tono; Splice: no | Aplicar `transpose` y warpear tonales; para Splice solo tras licenciar y aparecer en la carpeta local |

**C4.5 Recomendación sobre el crate: SUSTITUIR el lado online no oficial y COMPLEMENTAR con la carpeta local**

- **Conservar** (valor y bajo riesgo): el enlace a la página del sample o pack en splice.com (es un simple enlace), el ajuste de carpeta Splice + "Index now" (corrigiendo la ruta por defecto a `~/Splice` y detectándola), los mapeos `scale_of`/`splice_key` como utilidades de tonalidad y el diseño de fila (portada de pack, chips, onda, badge key/BPM).
- **Retirar de las builds de release o dejar sin compilar por defecto**: `client.rs` (GraphQL, descarga de previews), `descramble.rs`, `auth.rs` (OAuth con client id ajeno), `service.rs` y el ajuste "Search Splice" de Preferencias. Por ejemplo, detrás de una feature `unofficial-splice` desactivada, o sacando el crate del workspace. **Decisión del propietario**; este informe no modificó código.
- **Complementar** con la carpeta local de Splice (opción 1) como fuente principal, el panel lateral y el motor de preview (§D2), y con los plugins oficiales de Splice alojados en una pista (opción 0) para quien quiera el catálogo completo dentro del DAW.

---

## Parte D · Diseño

### D1. Opciones, ordenadas por viabilidad y riesgo legal

| # | Opción | Viabilidad | Riesgo legal | Notas |
|---|---|---|---|---|
| 0 | **Alojar los plugins oficiales de Splice** (Bridge / Sounds Plugin, VST3) en una pista de AI-Track | Alta (el host VST3 ya existe) | Muy bajo (uso del plugin tal cual; Splice lo ofrece para "cualquier DAW") | Falta: pasar tempo/transporte al plugin [S], aceptar `ExternalPaths` en el arrangement, probar que la ventana del plugin permite arrastrar ficheros al host. Sin control sobre el UX |
| 1 | **Indexar la carpeta local de Splice** (samples ya licenciados por el usuario) con filtros key/BPM y preview a tempo y tono | Alta (≈80 % de las piezas existe) | Bajo (ficheros del usuario, uso local; no hay red hacia Splice) | **Recomendada como núcleo**. El usuario descarga con la app/plugin de Splice. La limitación: solo escucha lo ya licenciado, no el catálogo completo |
| 2 | **Fuente online con API legal** (Freesound; luego otros con acuerdo) tras un trait de proveedores | Media | Bajo–medio (Freesound: comercial a negociar, licencia por sonido, clave por aplicación) | Da búsqueda online real con `bpm` y `tonality`; calidad dispar |
| 3 | **Acuerdo oficial con Splice** | Baja a corto plazo (sin programa abierto) | Nulo | Contactar; Splice integra por acuerdos bilaterales (Studio One, Pro Tools, Ableton). Realista con tracción |
| 4 | API privada/GraphQL + descrambling (`crates/splice`) | Técnicamente existe | **Alto** (§2.12(a),(j), §2.3.1.4(f), §11.4) | **Descartada** como base del producto: ver §C4.5 (retirar `client.rs`, `descramble.rs`, `auth.rs`, `service.rs` de las builds de release; conservar enlace, carpeta local y utilidades de tonalidad). Decisión del propietario; este informe no tocó código |
| 5 | Leer `sounds.db` de Splice | Baja (sin esquema documentado) | Medio (sin permiso explícito; reverse engineering §2.12(a)) | No depender de ello. El nombre de archivo y el análisis de audio bastan |

### D2. Opción recomendada: "Sonidos" = 1 + 2 (+0 opcional)

**Panel lateral persistente** (nuevo, a la izquierda del arrangement; la paleta Cmd+K conserva la búsqueda rápida y reutiliza el mismo componente de lista).

```
┌ Sonidos ─────────────────────┐
│ 🔍 buscar…           [Fuente ▾]│  Fuente: Librería · Carpeta Splice · Freesound
│ [Ajustar a la canción  ●]      │  maestro on/off
│  ♪ 128 BPM  ✓   ♯ Am (8A) ✓   │  chips que siguen al proyecto
│ [Loops][One-shots] [Kick][Bass]│  tipo / categoría (taxonomy.rs)
│ Key: ◉ Compatible ○ Exacta     │  KeyMatch
│ BPM: ±6 %  ☑ ½× / 2×           │  BpmFilter
│ ───────────────────────────── │
│ ▶ ▁▃▅▇▅▃▁ Bass_Am_128   Am 128 │  onda (peaks.rs), badge key/BPM
│              +0 st · 100 %     │  ajuste calculado
│ ▶ ▁▂▇▅▂▁▁ Pad_Em_124   Em 124  │
│              +2 st · 103 %     │
└──────────────────────────────┘
```

- **Búsqueda**: FTS5 existente (`samples_fts`) + filtros; botón "Search with Sound" local: selecciona un clip o rango, se detectan key/BPM con `aitrack-ai` (`detect_key`, `detect_bpm`) y se rellenan los chips; ordenar por similitud con la columna `embedding` ya presente [V: `db.rs` esquema].
- **Ajustar a la canción**: dos conmutadores, Tempo y Key. Por fila se muestra el ajuste calculado (`+N st`, `% de velocidad`). Si la canción no tiene escala (`project.scale == None`) el chip Key pide fijarla.
- **Interacciones**: clic = preview; ↑/↓ = preview tras 90 ms (ya existe `SampleAudition::SETTLE`); Espacio = pausa; arrastre al arrangement (ya existe `DraggedSample`) o Enter para añadir.
- Reglas de la plataforma: colores del tema, tiempos de `motion.rs`, iconos de `icons.rs`, espaciado múltiplo de 4 px.

**Modelo de datos** (aditivo; migración con `meta`):

```
samples            (ya existe)         -- metadatos de ficheros locales
roots.kind         + 'splice'          -- origen "carpeta Splice" (hoy: user/factory/cloud…)
remote_sounds(                           -- resultados online, NO son ficheros locales
  source TEXT, remote_id TEXT, name TEXT, preview_url TEXT, page_url TEXT,
  bpm REAL, key_root INT, scale_kind TEXT, duration REAL, tags TEXT,
  license TEXT, license_url TEXT, author TEXT, attribution TEXT,
  fetched_at INT, local_sample_id INT NULL,   -- se llena al descargar con licencia
  PRIMARY KEY(source, remote_id))
```

Trait `SoundSource { search(&Query)->Page; preview(&Item)->PathBuf; fetch(&Item)->PathBuf }` con implementaciones `LocalLibrary` y `Freesound`. Ninguna red en el hilo de UI ni de audio (hilos propios, como el `Service` actual). Se guarda siempre `license`/`author` para atribución.

**Cálculo de transposición (key de la canción P, key del sample S)**

1. Representar cada tonalidad por su máscara de pitch-classes (`Scale::mask()`, `model/src/scale.rs:126`), así relativas y modos (Dorian…) son un caso general.
2. Para `d ∈ [−6, +5]` calcular `fuera(d) = popcount(rotar(mask(S), d) & !mask(P))` (notas fuera de escala tras transponer).
3. Elegir `d` con `fuera` mínimo; desempate: (a) tónica de S transpuesta == tónica de P o relativa, (b) menor `|d|`, (c) preferir bajar (menos artefactos al estirar hacia arriba). Resultado `Fit{semitones, kind}` con `kind ∈ {Same, Relative, Transposed, Atonal}`.
   - Mismo escala → 0. **Am sobre C mayor → 0 (relativa)**, no +3 como daría `transpose_to` solo por la tónica (`query.rs:80`). **Am sobre Cm → +3**. Em sobre Am → +5 o −7 → se toma +5. Si `|d| > 4` mostrar aviso: los loops degradan.
4. One-shots tonales sin escala (solo `root_note`): `transpose_to(root_note % 12, P.root)` (ya implementado), o a la nota del acorde elegida.
5. Atonales (drums, FX): sin transposición. Sample sin key conocida: `—`, no se transpone y se marca para análisis.
6. Aplicación: preview con `ratio = 2^(d/12)` si es Repitch (one-shots) y con `warp_audio(Complex/Tones)` preservando duración para loops. Al soltar, `Warp { enabled, original_bpm, transpose: d, mode }` dentro del `AddSource` (un solo paso de undo).

**Tempo**: `r = BPM_proyecto / BPM_sample`; elegir entre `r`, `2r`, `r/2` el más cercano a 1 (hay `tempo_compat` con ½×/2× que lo respalda). Fuera de 0,7–1,4 → aviso/deshabilitar. Un loop sin BPM: usar `bpm_from_filename`/`detect_bpm`.

**Motor de preview sincronizado al compás**

1. *Render* en un worker (nunca en el hilo RT): decodificar → `warp_audio` con `original_bpm→tempo_proyecto` y `transpose=d` (modo según tipo: Beats para percusión, Complex/Tones para tonales) → buffer planar en caché `(hash, tempo, d, modo)`. Tiempo de render de 4–8 compases: **[S] decenas de ms**, medir. Mientras no esté listo, reproducir una versión Repitch aproximada o esperar al compás siguiente (se oculta la latencia).
2. *Arranque cuantizado*: si el transporte corre, empezar el loop en el siguiente **compás** (1–2 compases de loop) o tiempo (cortos), manteniendo la fase `pos = (tick_transporte − tick_inicio) mod longitud`. Si está parado, bucle libre a tempo del proyecto con rejilla virtual (como el preview de Ableton, §A1).
3. *Dónde suena*: **Fase 1 (M)**: ampliar `real/preview.rs` leyendo la posición del transporte desde un atómico del motor y ajustando el frame de inicio; el stream aparte tiene jitter ≤ un buffer (256 frames ≈ 5 ms) [S aceptable para escucha, medir]. **Fase 2 (L)**: bus de preview dentro del grafo del motor (comando `SetPreview{buffer, start_tick, loop}` por cola sin espera, como ya hace el preview con su cola) para fase exacta y misma latencia compensada que el resto.
4. Cambios de tempo/tono durante el preview → re-render y *crossfade* de 1 compás o 20 ms.
5. One-shots: bucle opcional de un compás; gain −6 dB como hoy (`GAIN` en `preview.rs`).

**Legal en la opción recomendada**: la carpeta de Splice contiene samples que el usuario ya licenció; AI-Track solo los lee. No se redistribuye nada. Los previews online de Freesound solo se cachean temporalmente y se citan su licencia y autor. Sin tokens ni credenciales de Splice. Para Freesound la clave de API la aporta el usuario (**[S]** inclusión de una clave propia en un binario open source choca con "claves confidenciales", por eso: clave propia por usuario o servidor intermedio, y negociar uso comercial).

### D3. Trabajos (S ≤ 1 día · M 2–4 días · L > 1 semana)

| # | Trabajo | Esfuerzo | Notas |
|---|---|---|---|
| 1 | Decisión legal sobre `crates/splice`: apagar por defecto/quitar de release, quitar el ajuste de Preferencias, corregir `NOTICE`; verificar licencia de splicerr; revisión legal de ToS | S | Prioritario; decisión del propietario |
| 2 | Contactar a Splice (partnership/permiso); registrar respuesta en este documento | S | Plazo incierto |
| 3 | Módulo `fit` (`crates/library` o `model`): `fit_key(P,S)`, `fit_tempo`, con tests (Am/C, Am/Cm, Em/Am, dorian, tritono, atonal) | S | Sustituye el uso de `transpose_to` en `recommend` |
| 4 | Origen "carpeta Splice": `RootKind::Splice`, ruta por defecto corregida (`~/Splice`, confirmar), botón "Indexar" (ya existe), badge de origen | S | Pedir ~50 nombres de archivo reales para afinar `parse.rs` |
| 5 | Preview sincronizado fase 1: worker de render con `warp_audio` + caché + arranque a compás | M | Base de todo |
| 6 | Preview sincronizado fase 2: bus de preview en el grafo del motor | L | Mejora de fase; opcional |
| 7 | Arrastre con ajuste: `add_sample_clip` aplica `transpose` y warp a loops y tonales; un solo undo | S | `workspace.rs:1905-1915` |
| 8 | Panel lateral "Sonidos": layout, filtros, chips, lista virtualizada, onda, estados | L | Reutilizar componentes de la paleta |
| 9 | "Search with Sound" local: key/BPM del clip seleccionado + similitud por embedding | M | Usa `crates/ai` |
| 10 | Trait `SoundSource` + `remote_sounds` + proveedor Freesound (clave de usuario, límites 60/min, atribución, caché de previews, OAuth2 para descargar) | L | Pendiente de acuerdo comercial |
| 11 | Hospedar Splice Bridge/Sounds Plugin: comprobar tempo/transporte hacia plugins y `ExternalPaths` en el arrangement | M | Vía 0, independiente |
| 12 | QA: sync de fase medido con loopback, test de no-asignaciones en callback, pruebas con nombres de Splice reales | M | Obligatorio por reglas de audio |

### D4. Riesgos

1. Legal: el crate no oficial (ver §C4); Splice puede cambiar su API o actuar contra clientes no oficiales.
2. Producto: sin acceso al catálogo online de Splice, la búsqueda cubre solo lo licenciado; el valor depende de que el usuario descargue con Splice.
3. Metadatos: el nombre del archivo puede no traer key/BPM fiables; hay que contar con análisis de audio (precisión sin medir) y con la confirmación del usuario.
4. Audio: estirar/transponer en tiempo real tiene artefactos con |d|>4 o tempo >±30 %; el render offline puede dar latencia perceptible.
5. Ableton añadirá más (Search with Sound, filtros por key): conviene no competir en catálogo sino en integración con nuestra librería inteligente offline.

---

## Fuentes

- Ableton, notas de versión Live 12: https://www.ableton.com/en/release-notes/live-12/
- Ableton, manual Live 12, "Working with the Browser" (§Splice): https://www.ableton.com/en/live-manual/12/working-with-the-browser/
- Splice, FAQ integración Ableton 12.3: https://support.splice.com/en/articles/12062860-faq-splice-integration-with-ableton-live-12-3
- Splice, descargar en Live 12.3: https://support.splice.com/en/articles/12059504-how-to-download-splice-samples-in-ableton-live-12-3
- Splice, dónde van las descargas: https://support.splice.com/en/articles/8652631-where-do-my-downloaded-samples-presets-midi-files-go
- Splice, Sounds Plugin (beta): https://support.splice.com/en/articles/12997860-faq-splice-sounds-plugin-now-in-beta ; https://support.splice.com/en/articles/12997591-getting-started-with-the-splice-sounds-plugin-now-in-beta ; https://support.splice.com/en/articles/13007631-how-to-download-samples-in-the-splice-sounds-plugin-now-in-beta
- Splice Bridge: https://support.splice.com/en/articles/8652855 ; https://splice.com/blog/how-to-use-splice-bridge
- Studio One: https://support.splice.com/en/articles/9953074-all-you-need-to-know-about-using-the-splice-studio-one-integration ; https://splice.com/blog/splice-integration-studio-one-update-november-2024/
- Pro Tools: https://splice.com/blog/splice-integration-pro-tools/
- Carta del CEO (sept-2025): https://splice.com/blog/letter-from-our-ceo-september-2025/
- Términos de Uso de Splice: https://splice.com/terms
- Índice de APIs (secundaria): https://apis.io/secondary-market/splice/
- splorganizer (sounds.db): https://github.com/ebai101/splorganizer ; splicerr: https://github.com/Robert-K/splicerr
- Freesound: https://freesound.org/docs/api/overview.html ; https://freesound.org/docs/api/resources_apiv2.html ; https://freesound.org/help/tos_api/
- BandLab Sounds: https://help.bandlab.com/hc/en-us/articles/360018942593-BandLab-Sounds
- Loopcloud: https://help.loopmasters.com/hc/en-us/articles/27012746051740-How-to-sync-Loopcloud-with-your-DAW
- LANDR (secundaria): https://apitracker.io/a/landr
