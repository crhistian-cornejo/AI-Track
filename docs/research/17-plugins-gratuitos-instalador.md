# 17 - Plugins gratuitos de terceros e instalador en un clic

> Investigación del 15 de noviembre de 2026. Solo catálogo y diseño: no se descargó ni se instaló nada, no se tocó código.
> Convención de evidencia: **[V]** = comprobado en la página oficial indicada durante esta investigación; **[S]** = supuesto, inferido o sacado de fuentes secundarias (blogs, tiendas, foros); **NC** = no confirmado.
> Aviso metodológico: las páginas se leyeron con una herramienta que resume el contenido (no con el HTML crudo), y varias webs devolvieron 403/404/429 (Valhalla, iZotope, Cymatics, Kilohearts EULA, Voxengo EULA, Waves). Antes de publicar cualquier entrada del manifiesto hay que repetir la comprobación con el instalador real en la mano. Esto no es asesoría legal.

---

## 0. Resumen ejecutivo

1. **Casi ningún freeware propietario permite redistribución.** TAL, Auburn Sounds y Youlean la prohíben por escrito [V]; Tokyo Dawn, Klanghelm, Xfer, Valhalla, Voxengo, Analog Obsession y la mayoría no dicen nada (**NC**, se asume que no). Por eso el carril de "empaquetar" lo ocupan casi solo proyectos **open source** (Surge XT, Dexed, Odin 2, sfizz, Dragonfly, Airwindows, Chowdhury DSP, ZL Audio) y **u-he**, cuyo EULA sí permite distribuir gratis con condiciones [V].
2. **Hallazgo incómodo sobre OTT:** la página de Xfer lista para macOS "VST, AU, AAX" y para Windows "VST, AAX", y menciona una "VST3 version" solo en el historial de cambios [V, https://www.xferrecords.com/freeware]. **No se pudo confirmar un VST3 en macOS ni hay CLAP.** Como AI-Track no aloja VST2 ni AU, OTT podría ser inutilizable en Mac. Sustituto con CLAP: Audio Damage **Evil Otto** (mailing list), y construir un dispositivo propio tipo OTT (hueco real, ver Parte B).
3. **Lo más valioso por categoría y viable hoy:** Surge XT, Dexed, Odin 2 y Zebralette 3 (sintes); TDR Nova, TDR Kotelnikov y ZL Equalizer/Compressor (dinámica y EQ); TAL-Reverb-4/Chorus-LX/Vocoder (VST3 + CLAP, descarga directa sin cuenta); X-Audio FAULTLINE e illformed Glitch² (stutter/glitch); Youlean y Voxengo SPAN (medición).
4. **Infraestructura reutilizable:** `crates/library/src/install.rs` ya hace descarga en hilo, cancelación, sha256, desempaquetado seguro y marcador de instalado; `hub.rs` ya modela "empaquetable" frente a "solo enlace"; `crates/ai/src/models.rs` aporta reanudación con `Range`, `installed.json` y aceptación de licencia. Faltan, para plugins: sha256 obligatorio, lista de hosts permitidos, permisos Unix/enlaces simbólicos en zip, extracción de dmg/pkg/msi sin ejecutar, comprobación de firma, recibos de instalación y re-escaneo del host. Diseño en la Parte C.

---

## 1. Los tres carriles (definición)

| Carril | Significado | Qué hace AI-Track |
|---|---|---|
| **L1 - Redistribuible** | La licencia permite repartir el binario (GPL/AGPL/BSD/MIT, o EULA con permiso explícito). | Podría ir en el instalador o en nuestro espejo. **Recomendación operativa:** aun así, descarga bajo demanda desde la release oficial con sha256 fijado, salvo piezas pequeñas; así no heredamos obligaciones de fuente GPL en el instalador principal. |
| **L2 - Gratis sin redistribución** | Gratis, sin cuenta, pero sin permiso de reparto. | Un clic descarga **desde el servidor del fabricante** (el usuario lo pide; no hay espejo nuestro) y se instala por usuario. |
| **L3 - Cuenta o gestor propio** | Exige cuenta, email, iLok, Native Access, Waves Central... | Solo enlace guiado a la página oficial; detectamos la instalación tras el re-escaneo. |

Conteo (de la tabla de la sección 2): **L1 = 19, L2 = 26, L3 = 18** (63 entradas), más 12 descartados con motivo (sección 2.5).

---

## 2. Catálogo (Parte A)

Columnas: **Fmt** = formatos que sirven a AI-Track (VST3/CLAP; si solo hay VST2/AU el plugin no sirve). **Mac/Win** = plataformas. **Licencia** y **Redistr.** = lo que se pudo leer. **Descarga** = URL y estabilidad. Tamaños: solo donde una fuente los dio.

### 2.1 Dinámica, EQ, saturación, medición

| id (propuesto) | Fabricante / plugin | Categoría | Fmt | Mac / Win | Licencia / redistribución | Descarga e instalador | Carril |
|---|---|---|---|---|---|---|---|
| `xfer-ott` | Xfer Records OTT 1.37 | multibanda tipo OTT | VST3 solo confirmado en historial; en Mac la lista dice VST/AU/AAX; sin CLAP [V] | ARM nativo citado solo para AAX; universal NC / Win sí | "freeware"; sin EULA visible; redistr. NC (asumir no) [V https://www.xferrecords.com/freeware] | `https://www.xferrecords.com/product_downloads/24/freeware` (Mac, `Install_Xfer_OTT_137.dmg`) y `.../25/freeware` (Win, `.exe`); ruta no versionada; cuenta NC. Foro: problema de recall VST2->VST3 en macOS Tahoe [V https://www.xferrecords.com/forums/posts/84301] | L2 (comprobar VST3 en Mac antes) |
| `audiodamage-evil-otto` | Audio Damage Evil Otto | multibanda tipo OTT | VST3, CLAP, AU, LV2, AAX [S: synthanatomy 2026-03] | Mac Apple Silicon+Intel / Win / Linux [S] | gratis con mailing list; licencia NC | enlace por email | L3 |
| `tdr-nova` | Tokyo Dawn Labs TDR Nova 2.2.3 | EQ dinámico (alt. Pro-Q/Pro-MB) | VST3, VST2, AU, AAX; sin CLAP [V https://www.tokyodawn.net/tdr-nova/] | Apple Silicon nativo desde 2.1.4; universal NC / Win | sin EULA en la ficha; términos del sitio prohíben "distribute" su material y limitan a uso personal no comercial [V https://www.tokyodawn.net/terms-and-conditions/ cl. 11-12]; redistr.: NO | zips versionados estables: `https://www.tokyodawn.net/labs/Nova/2.2.3/TDR Nova.zip` (Mac), `... (installer).zip` y `... (no installer).zip` (Win) [V]; hay firma `.asc` + clave pública solo para Linux | L2 |
| `tdr-kotelnikov` | TDR Kotelnikov 1.6.7 | compresor de bus/mastering (alt. Pro-C, The Glue) | VST3 todas las plataformas [V https://www.tokyodawn.net/tdr-kotelnikov/] | Apple Silicon nativo desde 1.6.3 / Win 7+ | como TDR | `https://www.tokyodawn.net/labs/Kotelnikov/1.6.7/TDR Kotelnikov.zip` y variantes Win [V] | L2 |
| `tdr-slickeq` | TDR VOS SlickEQ 1.3.9 | EQ de color | VST3 [V] | Apple Silicon nativo desde 1.3.6 / Win | como TDR | `https://www.tokyodawn.net/labs/SlickEQ/1.3.9/TDR VOS SlickEQ.zip` [V] | L2 |
| `tdr-molotok` | TDR Molotok 1.0.7 | compresor | VST3 [V] | Apple Silicon: solo consta AAX / Win | **la página no dice que sea gratis (NC)** | `https://www.tokyodawn.net/labs/Molotok/1.0.7/...` | L2 (pendiente confirmar gratuidad) |
| `klanghelm-dc1a` | Klanghelm DC1A 3.5.0 | compresor | VST3 NC | "todos nativos en Apple Silicon" (anuncio 2021) [S] | "DC1A is freeware" [V https://klanghelm.com/contents/news]; redistr. NC | instaladores Mac/Win en su web; tamaño NC | L2 |
| `klanghelm-ivgi` | Klanghelm IVGI 2.5.0 | saturación | VST3 NC | idem | "Freeware" [V] | idem | L2 |
| `klanghelm-mjuc-jr` | Klanghelm MJUC jr. | compresor variable-mu (alt. glue) | VST3 NC | idem | gratis [V https://klanghelm.com/MJUC]; licencia NC | enlace en su página | L2 |
| `analogobsession` | Analog Obsession (BUSTERse, LALA, Rare...) | compresor SSL de bus, EQ | VST3, AU, AAX; sin CLAP [V https://www.analogobsession.com/faq] | FAQ: macOS 10.11-13.x, Win 10-11; **soporte Mac reciente/ARM NC** | "free to use. Support on Patreon is optional" [V]; EULA NC | página del fabricante | L2 |
| `loudmax` | Thomas Mundt LoudMax 1.47 | limitador (alt. Pro-L) | VST3 (Win y Mac) [V https://loudmax.blogspot.com/] | Mac Universal 2 notarizado / Win | freeware no comercial; "may not distribute or modify" sin permiso [V]; redistr.: NO | zips en **Dropbox** (frágil): `LoudMax_1_47_Mac_notarized.zip`, `LoudMax_v1_47_WIN_VST3.zip` | L2 (URL frágil) |
| `youlean-loudness-meter-2` | Youlean Loudness Meter 2 (Free) 2.5.15 | medidor LUFS | VST3 (+VST2, AUv2, AAX); sin CLAP [V https://youlean.co/download-youlean-loudness-meter/] | Mac Intel+ARM; Win x64 y ARM64 | freeware con funciones limitadas; no redistribuir sin permiso escrito [S]; EULA enlazada, no leída | directa sin cuenta: `https://cdn.youlean.co/wp-content/uploads/2026/09/Youlean-Loudness-Meter-2-V2.5.15-Windows.zip` y `...-macOS.dmg` (la URL cambia con la versión) [V] | L2 |
| `voxengo-span` | Voxengo SPAN 3.24 | analizador de espectro | VST3 solo en Win; **el dmg de Mac trae AU/AAX/VST (sin VST3)** [V https://www.voxengo.com/product/span/] | Apple Silicon desde 3.11 [S] / Win x64 | Condiciones de uso prohíben copiar a otros servidores salvo que la licencia lo permita [V https://www.voxengo.com/conditions-of-use/]; redistr. NC | `https://www.voxengo.com/files/VoxengoSPAN_324_Win32_64_VST_VST3_AAX_setup.exe`; Mac solo AU/VST2: **no sirve en Mac** | L2 (solo Windows) |
| `airwindows-consolidated` | Airwindows (Chris Johnson) / build de Paul Walker | ~500 efectos | CLAP, VST3, AU, LV2 [V https://github.com/baconpaul/airwin2rack] | Mac universal (dmg firmado) / Win x64 | código Airwindows **MIT** [V https://github.com/airwindows/airwindows]; **el binario DAW arrastra dependencias GPL3 (JUCE, VST3 SDK)** [V README de airwin2rack] | release rodante tag `DAWPlugin` (actualizada semanalmente): `airwindows-consolidated-macOS-2026-11-08-316a048.dmg` (~49,5 MB), `...Windows.zip` (~13,6 MB), `...64bit-setup.exe` (~10,9 MB), más `artifact_sha256sum.txt`; **nombres con fecha y hash: resolver por API de GitHub** [V https://github.com/baconpaul/airwin2rack/releases/tag/DAWPlugin] | L1 (tratar el binario como GPL) |
| `zl-equalizer-2` | ZL Audio ZL Equalizer 2 1.4.1 | EQ (alt. Pro-Q) | VST3 y LV2 [V README]; CLAP NC | dmg **por arquitectura** (arm64 11,2 MB; x86-64 12,5 MB) / msi (10,0 MB) | **AGPL-3.0** [V https://github.com/ZL-Audio] | GitHub Releases, nombres estables por tag: `ZL.Equalizer.2-1.4.1-macOS-arm64.dmg`, `...-Windows-x86-64.msi` | L1 (AGPL: solo bajo demanda) |
| `zl-compressor` | ZL Audio ZL Compressor 0.6.1 | compresor | VST3 [V]; CLAP NC | dmg arm64 12,5 MB / msi 10,9 MB | AGPL-3.0 | `ZL.Compressor-0.6.1-macOS-arm64.dmg`, `...-Windows-x86-64.msi` | L1 (AGPL) |
| `zl-limiter` | ZL Audio ZL Limiter 0.0.1 (**prerelease**) | limitador | VST3 NC | dmg arm64 5,0 MB / msi 5,6 MB | AGPL-3.0 | release del 2026-10-31 | L1 (AGPL, no recomendado aún) |
| `byod` | Chowdhury DSP BYOD 1.3.0 | saturación modular | VST3, CLAP, AU, AAX, LV2 [V https://chowdsp.com/products.html] | `BYOD-Mac-1.3.0.dmg` 89,1 MB / `BYOD-Win-64bit-1.3.0.exe` 26,4 MB | GPL-3.0 [V API GitHub] | GitHub Releases | L1 (GPL, bajo demanda) |
| `chowtapemodel` | Chowdhury DSP ChowTapeModel 2.11.4 (2023) | emulación de cinta | formatos NC | dmg 28,7 MB / exe 10,6 MB | GPL-3.0 [V] | GitHub Releases | L1 |
| `chowmultitool` | Chowdhury DSP ChowMultiTool 1.1.0 | EQ/waveshaper/split | VST3/CLAP NC (SVF solo CLAP) | dmg 30,6 MB / exe 7,4 MB | GPL-3.0 [V] | GitHub Releases | L1 |
| `chowkick` | Chowdhury DSP ChowKick 1.2.0 | bombo sintético | formatos NC | NC | BSD-3-Clause [V] (binario puede incluir JUCE) | GitHub Releases | L1 |
| `chowmatrix` | Chowdhury DSP ChowMatrix 1.3.0 | delay matricial | formatos NC | NC | BSD-3-Clause [V] | GitHub Releases | L1 |
| `softube-saturation-knob` | Softube Saturation Knob | saturación | VST3, AU, AAX; sin CLAP [V https://www.softube.com/saturationknob] | NC | gratis vía **Softube Central + cuenta Softube + cuenta iLok** [V] | solo enlace | L3 |
| `slate-free-collection` | Slate Digital Free Collection (Fresh Air, VBC Red...) | 16 plugins | VST3, AU, AAX [V https://www.slatedigital.com/free-collection/] | NC | cuenta gratuita "Complete Access" + instalador propio (Hub) [V] | solo enlace | L3 |
| `waves-free-pack` | Waves Free Plugin Pack (abr-2025) | 7-9 plugins (Lil Tube, V-EQ3, V-Comp, IR Live, Flow Motion...) | VST3/AU/AAX [S]; sin CLAP | NC | cuenta Waves + Waves Central, una licencia por cuenta [S, waves.com devolvió 404] | solo enlace | L3 |
| `melda-mfreefxbundle` | MeldaProduction MFreeFXBundle | 38 efectos (MAutoPitch, MAnalyzer, MCompressor...) | VST3/AU/AAX [S]; sin CLAP | NC | "completely free" [V https://www.meldaproduction.com/MFreeFXBundle]; EULA NC (404) | instalador propio de Melda | L3 |
| `polyverse-wider` | Polyverse Music Wider | ensanchador | VST2/VST3/AU/AAX [V https://polyversemusic.com/products/wider/] | Win64 + macOS 10.9+ 64-bit; ARM NC | "100% FREE" [V]; pide nombre y email y envía enlace | email | L3 |
| `izotope-imager` | iZotope Ozone Imager 2 | ensanchador/medidor | VST3/AU [S] | NC | cuenta iZotope + Product Portal [S] | solo enlace | L3 |

### 2.2 Reverb, delay, modulación, voz

| id | Fabricante / plugin | Categoría | Fmt | Mac / Win | Licencia / redistribución | Descarga e instalador | Carril |
|---|---|---|---|---|---|---|---|
| `valhalla-supermassive` | Valhalla DSP Supermassive 5.0.0 "Sirius" (nov-2025) | reverb/delay | VST3, AU, AAX [S]; sin CLAP | Mac universal [S] / Win [S] | freeware; **EULA NC** (valhalladsp.com devuelve 403) | enlaces Mac/Win en su web; pkg/exe [S]; sin registro [S] | L2 |
| `valhalla-freqecho` | Valhalla FreqEcho | delay con frequency shifter | NC | idem | idem | idem | L2 |
| `valhalla-spacemodulator` | Valhalla Space Modulator | chorus/flanger/phaser | VST3, AU, AAX [S] | idem | gratis para todos desde 2020 [S] | idem | L2 |
| `tal-reverb-4` | TAL Software TAL-Reverb-4 4.0.4 (2023) | reverb de placa | VST, VST3, AU, AAX, **CLAP** [V https://tal-software.com/products/tal-reverb-4] | Universal 2 (macOS 10.9+) / Win 7+ x64 | "share, resell, sublicense, rent, lease, or otherwise redistribute" prohibido; uso comercial de productos gratuitos permitido [V https://tal-software.com/disclaimer sec. 7 y 13]; la ficha no declara que sea gratis (la gratuidad es [S]) | zips directos sin cuenta: `install_*.zip` (Win) y `*-installer_macos.zip` (Mac) | L2 |
| `tal-chorus-lx` | TAL-Chorus-LX 1.6.3 | chorus Juno-60 | VST3, CLAP [V] | Universal 2 / Win | idem | idem | L2 |
| `tal-filter-2` | TAL-Filter-2 3.1.2 | filtro sincronizado / trance gate | VST3, CLAP [V https://tal-software.com/products/tal-filter] | idem | idem | idem | L2 |
| `tal-vocoder` | TAL-Vocoder-2 3.0.4 | vocoder | VST3, CLAP [V https://tal-software.com/products/tal-vocoder] | idem | idem | idem | L2 |
| `tal-noisemaker` | TAL-NoiseMaker 5.0.6 (2023) | sinte virtual analógico | VST3, CLAP [V https://tal-software.com/products/tal-noisemaker] | Universal 2 / Win x64 | idem (redistr.: NO) | `https://tal-software.com/downloads/plugins/install_TAL-NoiseMaker.zip` y `.../TAL-NoiseMaker-installer_macos.zip` [V] | L2 |
| `dragonfly-reverb` | Michael Willis Dragonfly Reverb 3.2.10 | 4 reverbs | **VST3/CLAP NC** (topics: lv2, vst) | `dragonfly-reverb-mac-universal-3.2.10.zip`, `...-win64-3.2.10.zip` | GPL-3.0 [V https://github.com/michaelwillis/dragonfly-reverb] | GitHub Releases (12 assets) | L1 |
| `kilohearts-essentials` | Kilohearts Essentials (34 efectos incl. Tape Stop, Trance Gate, Formant Filter, Pitch Shifter, Reverb, Limiter, Transient Shaper) | multi-efecto | VST2+VST3 [V parcial https://kilohearts.com/products/one]; CLAP NC | NC | "Free with an account" [V https://kilohearts.com/products/kilohearts_essentials]; EULA NC (404) | cuenta + Kilohearts Installer (se autoactualiza) | L3 |
| `babyaudio-freebies` | Baby Audio: Warp (x Andrew Huang), Magic Switch, Magic Dice, Pitch Drift, Beat Slammer | varios | NC [S: VST3/AU/AAX] | NC | **cuenta Baby Audio gratuita** [V https://babyaud.io/freebies] | solo enlace | L3 |
| `izotope-vinyl` | iZotope Vinyl | lo-fi/vinilo | VST3/AU/AAX/VST2 [S] | Apple Silicon NC | gratis con cuenta + Product Portal [S] | solo enlace | L3 |
| `auburn-graillon-free` | Auburn Sounds Graillon 3 Free (también Inner Pitch, Lens, Couture, Panagement... Free) | voz: pitch, formante | VST2/VST3/AU/AAX/CLAP en la línea [V https://www.auburnsounds.com/llms.txt]; **en la ficha de Graillon Free: VST, AAX, AU, LV2 (VST3/CLAP NC)** | Mac Intel+ARM / Win | licencia personal "for evaluation"; **prohibido "sell or distribute the Free Editions"** [V https://www.auburnsounds.com/terms.html] | zip sin cuenta (`Graillon-FREE-3.2.zip`) | L2 |
| `pitchmunk` | Vox Samples Pitchmunk | pitch + formante | VST3/AU (fuentes discrepan) [S] | NC | gratis [S] | NC | L2 (sin verificar) |

### 2.3 Stutter, glitch, beat repeat, half-time, tape stop

Para "stutter house" el hueco principal (no hay nada en `crates/stock`) se cubre así:

| id | Fabricante / plugin | Categoría | Fmt | Mac / Win | Licencia / redistribución | Descarga e instalador | Carril |
|---|---|---|---|---|---|---|---|
| `xaudio-faultline` | X-Audio FAULTLINE 1.0 | beat repeat, freeze, glitch, bitcrush; slice repeats 1/64 a rolls half-time | AU + VST3 en Mac; VST3 en Win; sin CLAP [V https://xaudio.store/faultline.html] | Mac universal firmado y notarizado / Win | freeware; términos NC | zip con `.pkg` (~6,5 MB [S]) tras dar nombre y email | L3 |
| `illformed-glitch2` | illformed (dblue) Glitch 2.1.5 | multiefecto glitch/stutter | VST3 (Win/Mac/Linux), VST2, AU solo Mac; sin CLAP [V https://illformed.com/glitch/] | Mac Intel+Apple Silicon (OS X 10.11+) / Win 7+ | gratis desde ago-2026 en memoria de Kieran Foster [V, Synthtopia 2026-08-04]; EULA no en la página, "All rights reserved"; redistr. NC. **Confirmar que la gratuidad es permanente** | zip por plataforma `Glitch_2_1_5_*_Free.zip`, sin cuenta | L2 |
| `tape-fiasco` | Jonas Eriksson Tape Fiasco | stutter + varispeed + granular | formatos NC (fuentes discrepan) | ~8 MB Mac, ~4 MB Win [S] | freeware [S]; redistr. NC | erikssonjonas.com, sin cuenta [S] | L2 (sin verificar) |
| `glitchmachines-hysteresis` | Glitchmachines Hysteresis 1.4.0 | glitch delay | VST3, AU [V https://glitchmachines.com/products/hysteresis/] | macOS 11+ / Win 11 | gratis vía "carrito"; cuenta NC; licencia NC | descarga desde la tienda | L3 |
| `wavesfactory-free` | Wavesfactory: Cassette Transport (tape start/stop), SnareBuzz, Flash | efectos | VST, VST3, AAX, AU [V https://www.wavesfactory.com/free-plugins/] | macOS y Windows | pide email para enviar enlaces [V]; redistr. NC | email | L3 |
| `cymatics-free` | Cymatics Diablo Lite (transient), Origin (lo-fi) | efectos | VST/AU/AAX; VST3 NC [S] | NC | gratis con email [S] | email | L3 |
| (dentro de Essentials) | Kilohearts **Tape Stop** y **Trance Gate** | tape stop, gate rítmico | ver Essentials | | | | L3 |

No hay un "stutter edit" gratuito, serio y VST3 de primer nivel salvo FAULTLINE y Glitch²; Kilohearts Essentials **no** incluye stutter/beat-repeat [V].

### 2.4 Sintes y samplers

| id | Fabricante / plugin | Categoría | Fmt | Mac / Win | Licencia / redistribución | Descarga e instalador | Carril |
|---|---|---|---|---|---|---|---|
| `surge-xt` | Surge Synth Team Surge XT 1.3.4 (estable; nightlies más nuevos) | híbrido, enorme librería de presets | VST3 + CLAP (+AU Mac, LV2 Linux); sin VST2 [V https://surge-synthesizer.github.io/] | Mac **universal** / Win 64 | **GPL-3.0** [V https://github.com/surge-synthesizer/surge/blob/main/LICENSE]; el README indica que los builds VST2 no se pueden redistribuir | GitHub `surge-synthesizer/releases-xt` tag 1.3.4: `surge-xt-macOS-1.3.4.dmg` (417 MB), `surge-xt-macos-1.3.4-pluginsonly.zip` (187 MB), `surge-xt-win64-1.3.4-pluginsonly.zip` (49 MB), `...-setup.exe` (183 MB) [V vía API] | L1 |
| `dexed` | Pascal Gauthier Dexed 1.1.0 (2026-10-31) | FM DX7 | CLAP confirmado desde 0.9.7; VST3 probable [S] | `Dexed-1.1.0-macOS.zip` (22,7 MB), `Dexed-1.1.0-win.zip` (12,3 MB); universal NC | GPL-3.0 [V]; motor msfa Apache-2.0 | `https://github.com/asb2m10/dexed/releases/download/v1.1.0/Dexed-1.1.0-macOS.dmg` [V] | L1 |
| `odin2` | TheWaveWarden Odin 2 (web 2.4.0, GitHub 2.4.1) | híbrido | Mac VST3/AU/CLAP; Win VST3/CLAP [V https://thewavewarden.com/pages/odin-2] | Apple Silicon NC | GPL-3.0 [V]; fuente Aldrich bajo OFL 1.1 | web: enlaces Dropbox (frágil); GitHub: `Odin2.4.1MacInstaller.pkg` (132 MB), `Odin2.4.1WinInstaller.exe` (41 MB), en la release "NightlyDevel" | L1 |
| `sfizz` | sfztools sfizz 1.2.3 | sampler SFZ | AU, LV2, PD, **VST3** [V repo sfizz-ui]; sin CLAP | pkg universal / exe 64 bit | BSD-2-Clause [V]; **repo archivado 2026-06-21** | https://sfz.tools/sfizz/downloads | L1 (sin mantenimiento) |
| `cardinal` | DISTRHO Cardinal 26.02 | modular tipo VCV Rack | VST3, CLAP (en desarrollo), AU, LV2 [V] | `Cardinal-macOS-universal-26.02.pkg` (974 MB) / `Cardinal-win64-26.02-installer.exe` (369 MB) | GPL-3.0-or-later; parte del arte con licencias CC-NC-ND y propias (docs/LICENSES.md) | GitHub | L1 (no empaquetar: 1 GB) |
| `zebralette-3` | u-he Zebralette 3.0.0 (2025-12-10) | wavetable | CLAP, VST3 (+AU Mac); problema de carga en Cubase/Nuendo [V https://u-he.com/products/] | Apple Silicon sí / Win | **EULA u-he: "freely distributed" con condiciones** (licencia adjunta, gratis, sin modificar, sin ingeniería inversa, sin quitar avisos) [V https://u-he.com/about/eula/] | directas, sin cuenta visible: `https://dl.u-he.com/releases/Zebralette3_300_20399__Mac.zip` y `.../Zebralette3_300_20399_Win.zip` (doble guion bajo en Mac, tal cual en la web) | L1 |
| `tyrell-n6` | u-he TyrellN6 3.0.0 (beta pública) | analógico estilo Juno | CLAP, VST3, AU (Mac); CLAP+VST3 (Win) [V] | Apple Silicon / Win | EULA u-he | `https://dl.u-he.com/releases/TyrellN6_300_public_beta_16976_Mac.zip` y `..._Win.zip` | L1 |
| `podolski` | u-he Podolski 1.2.3 | sinte | VST3, AUv2; sin CLAP [V] | Apple Silicon sí | EULA u-he | `https://dl.u-he.com/releases/Podolski_123_12092_Mac.zip` y `..._Win.zip` | L1 |
| `protoverb` | u-he Protoverb 1.0.1 | reverb experimental | VST3, AUv2; sin CLAP [V] | idem | EULA u-he | `dl.u-he.com/releases/Protoverb...` (nombre exacto NC) | L1 |
| `obxd` | discoDSP / reales OB-Xd 2.20 (2026-08-18) | Oberheim OB-X | VST3/AU/LV2; sin CLAP | Apple Silicon NC | README: "free for any use, including commercial work" [V https://github.com/reales/OB-Xd]; **redistr. NC** | GitHub Releases: `OB-Xd.2.20.pkg` (39,7 MB), `Obxd220.exe` | L2 |
| `sforzando` | Plogue sforzando 1.982 (2026-04-14) | player SFZ | VST3, CLAP, AU, AAX [V https://www.plogue.com/downloads.html] | Apple Silicon nativo / Win 10 x64 | gratis; EULA NC (asumir no) | `https://s3.amazonaws.com/sforzando/WIN_sforzando_v1.982.exe` y `.../MAC_sforzando_v1.982.dmg` [V] | L2 |
| `vital` | Matt Tytel Vital (Basic gratis: 75 presets, 25 wavetables) | wavetable (el mejor para EDM de esta lista) | VST3, CLAP [S foros] | Intel o Apple Silicon (macOS 10.15+) / Win 10+ [V https://vital.audio/] | **binarios NO son GPL**; el repo `mtytel/vital` es GPL-3.0 solo en el código y prohíbe usar el nombre Vital en derivados y redistribuir los presets gratuitos [V https://github.com/mtytel/vital] | **cuenta obligatoria** en account.vital.audio | L3 |
| `decent-sampler` | Decent Samples Decent Sampler 1.42.1 | player de librerías .dspreset (ecosistema Pianobook) | VST3, VST2, AAX, AU; sin CLAP [V https://www.decentsamples.com/product/decent-sampler-plugin/] | Intel+ARM nativo / Win 10+ | EULA solo habla de librerías; plugin NC | formulario con email; hay versión sin instalador en Win | L3 |
| `splice-instrument` | Splice INSTRUMENT (heredero de Spitfire LABS; LABS legacy con "sunset" 31-oct-2026) | instrumentos gratis | VST3, AU, AAX [V soporte Spitfire/Splice] | NC | **cuenta Splice** | solo enlace: https://splice.com/instrument | L3 |
| `ni-komplete-start` | Native Instruments Komplete Start (con Kontakt Player) | sintes y samplers | VST3/AU/AAX [S] | Apple Silicon sí | **Native Access + cuenta NI** (~17 GB) | solo enlace | L3 |

### 2.5 Descartados (con motivo)

| Plugin | Motivo |
|---|---|
| Limiter No6 (vladg/sound) | Solo VST2 (Win 32/64; Mac 32 bit), sin soporte [S, distribuido por Tokyo Dawn con ese aviso] |
| Tritik Krush | De pago; la demo silencia audio a intervalos [V https://tritik.com/krush] |
| TAL-Dub-X | De pago desde v1.8.0 (serial; demo con ruido) [V] |
| Devious Machines | Sin gratuitos, solo demos [S] |
| BLEASS | Sin efectos gratuitos relevantes [S] |
| ToneBoosters (legacy) | Solo VST2/AU [S] |
| Helm | Archivado desde 2022; formatos Mac/ARM dudosos [V] |
| Monique Monosynth | Solo binarios "Nightly", sin versión estable [V] |
| Sitala v1 | Sin VST3 ni Apple Silicon (v2 de pago) [S] |
| GSnap | Solo VST2 y Windows [S] |
| Full Bucket Music | Gratuitos poco relevantes (efectos menores) y licencia NC [V] |
| Kilohearts Snap Heap / Multipass | De pago |

### 2.6 La marca "aterboys" (transcripción de voz dudosa)

No se encontró ninguna marca llamada "Waterboys"/"Water Boys" (búsqueda web y Reddit sin resultados) [V negativo]. Candidatos, de mayor a menor encaje:

1. **Si es fonética: Soundtoys "Little AlterBoy"** (plugin de voz: pitch + formante + saturación). "AlterBoy(s)" suena a "aterboys". Es de pago (iLok); alternativas gratuitas: Auburn Sounds Inner Pitch/Graillon Free y Pitchmunk [S]. Soundtoys vende un *bundle*, no alternativas a bundles.
2. **Si es la descripción ("alternativas a todos los bundles"): Airwindows** (cientos de efectos open source, MIT, uno por cada clásico: compresores, EQ, cinta, saturadores) es el que mejor encaja y además **sí es empaquetable** (con la salvedad de la GPL del build DAW). Le siguen **Kilohearts Essentials** (34 efectos modulares gratis) y **MeldaProduction MFreeFXBundle** (38 efectos).
3. Analog Obsession y Black Rooster Audio (clones baratos o gratuitos de LA-2A, SSL...) encajan peor.

Conjetura sin evidencia directa. Hay que pedir al dueño el nombre escrito. Mientras tanto, el catálogo cubre las tres lecturas.

---

## 3. Equivalencias (Parte B)

Dispositivos propios consultados en `crates/stock/src/lib.rs` (registro) y cabeceras de `crates/stock/src/fx/*` y `instrument/*`.

| Producto comercial citado | Mejor alternativa gratuita de terceros | Equivalente propio actual | Estado |
|---|---|---|---|
| **Xfer OTT** (gratis, pero ver sección 0) | Xfer OTT (si hay VST3 Mac); Audio Damage Evil Otto (CLAP); TDR Nova (EQ dinámico); Kilohearts Compressor/Dynamics (L3) | `fx.multiband` es **solo crossover de 3 bandas con ganancia/solo/delta**, sin compresión | **Hueco: hacer un "Multiband Dynamics" estilo OTT** (compresión descendente y ascendente por banda, Depth/Time/Mix) |
| **Cytomic The Glue** | TDR Kotelnikov; Klanghelm MJUC jr. / DC1A; Analog Obsession BUSTERse | `fx.compressor` con estilo **Glue** (RMS >=60 %, release dependiente del programa) y `fx.saturator.console` | Cubierto (afinar el estilo Glue contra Kotelnikov) |
| **FabFilter Pro-Q** | ZL Equalizer 2 (AGPL); TDR Nova; TDR VOS SlickEQ | `fx.eq` (dinámico, M/S, FIR de fase lineal) | Cubierto |
| **FabFilter Pro-C 2** | ZL Compressor; TDR Kotelnikov | `fx.compressor` (Clean/Punch/Glue/Opto) | Cubierto |
| **FabFilter Pro-L 2** | LoudMax; ZL Limiter (prerelease); Limiter No6 solo VST2 | `fx.limiter` (true peak, master) | Cubierto |
| **FabFilter Pro-MB / Saturn** | TDR Nova; BYOD; ChowMultiTool; Airwindows | `fx.saturator` (multibanda), `fx.multiband` | Parcial (falta multibanda dinámica) |
| **FabFilter Pro-R / Valhalla** | Valhalla Supermassive; TAL-Reverb-4; Dragonfly; Protoverb | `fx.reverb` (plate/hall/shimmer) | Cubierto; las alternativas aportan carácter |
| **Waves (paquete)** | Waves Free Pack (L3: V-EQ3, V-Comp, Lil Tube...); Slate Free Collection (L3); Melda MFreeFXBundle; Airwindows | EQ, compresor, saturator, tape, amp, console | Cubierto en lo esencial |
| **Waves L1/L2 / Maximizer** | LoudMax; ZL Limiter | `fx.limiter`, `fx.clipper` | Cubierto |
| **Waves SSL / API / Abbey Road** | Analog Obsession BUSTERse/LALA; Airwindows (Console, ADT...) | `fx.saturator.console` | Parcial |
| **Baby Audio** (Magic Switch, Smooth Operator, Crystalline) | Baby Audio freebies (L3); Kilohearts Phaser/Chorus | `fx.chorus`, `fx.saturator.tape` | Parcial |
| **Soundtoys Little AlterBoy / Auto-Tune** | Auburn Graillon Free / Inner Pitch Free; MAutoPitch (Melda) | **ninguno** | **Hueco: pitch/formante y afinador** |
| **TAL / Vocoder** | TAL-Vocoder-2 | **ninguno** | Hueco (vocoder) |
| **Stutter Edit / Gross Beat / HalfTime / ShaperBox** | FAULTLINE; Glitch²; Hysteresis; Tape Stop (Kilohearts); Cassette Transport | `fx.shaper` (envolvente de volumen/filtro/pan sincronizada) cubre el "ducking/pump" tipo LFOTool/Kickstart | **Hueco: stutter/beat-repeat/half-time/tape-stop** |
| **Serum** | Vital (L3, cuenta); Surge XT; Zebralette 3 | `instrument.wavetable` | Cubierto; Vital/Surge aportan presets |
| **Massive / FM8** | Surge XT; Dexed | `instrument.wavetable`, `instrument.vault` (FM, electric piano, órgano) | Cubierto |
| **Omnisphere / Analog Lab** | Surge XT; Odin 2; Vital | `instrument.vault` | Parcial |
| **Kontakt / Spitfire LABS** | Decent Sampler; sfizz; sforzando; Splice INSTRUMENT (L3) | `instrument.sampler`, `instrument.sfz` | Cubierto (SFZ propio; sfizz solo como extra) |
| **Roland TB-303 / TR-808** | (Surge/Odin como alternativas generales) | `instrument.acid303`, `instrument.kick808` | Cubierto |
| **SPAN / Youlean / iZotope Insight** | Voxengo SPAN (solo Win en VST3), Youlean Loudness Meter 2 | `fx.analyzer` (espectro, loudness, meter) | Cubierto |
| **Ozone Imager / Wider** | Polyverse Wider (L3), Imager 2 (L3) | `fx.midside`, `fx.utility` (ancho/mono) | Cubierto |
| **Little Lo-Fi / iZotope Vinyl** | Vinyl (L3); Airwindows | no se encontró bitcrusher/vinilo dedicado | Hueco menor |

### 3.1 Qué esto implica

- Los **huecos reales** que ningún plugin de terceros gratuito, redistribuible y VST3/CLAP cubre bien: OTT/multibanda dinámica, stutter/beat-repeat/half-time/tape-stop, pitch/formante y vocoder. Conviene hacerlos propios (no dependen de licencias) y usar los de terceros como complemento.
- Todo lo de terceros es **adicional**: la base de AI-Track debe seguir siendo completa sin él.

---

## 4. Infraestructura existente (auditoría para la Parte C)

| Pieza | Qué ofrece | Qué falta para plugins |
|---|---|---|
| `crates/library/src/install.rs` (408 líneas) | `InstallJob::spawn` en hilo con cola de eventos (`Progress{Downloading, Verifying, Unpacking}`, `Done`, `Failed`, `Cancelled`); cancelación; `download` en streaming con `ureq`; `verify_sha256`; `unpack` de zip/tar.gz/tar.xz/7z; defensa contra rutas que escapan (`enclosed_name`, `unpack_in`, canonicalize en 7z); `normalize` con `include`/`exclude`/`strip_prefix`; carpeta `.work` y renombrado final; `ATTRIBUTION.txt` y marcador `.aitrack-pack`. | (1) sha256 **opcional** (`if !pack.sha256.is_empty()`): para plugins debe ser obligatorio. (2) `download` sin reanudación, sin lista de hosts, sin tope de tamaño, redirecciones a cualquier host. (3) `unpack_zip` **no restaura permisos Unix ni enlaces simbólicos** (un `.vst3`/`.component` de macOS necesita el bit de ejecución y los symlinks de `Versions/`). (4) Sin límites anti zip-bomb (nº de ficheros, ratio, bytes totales). (5) No extrae dmg/pkg/msi/exe. (6) Sin firma/Team ID. (7) Sin recibo para desinstalar/actualizar. |
| `crates/library/src/hub.rs` (314 líneas) | `Manifest` TOML compilado con `include_str!` (`content/soundbank/manifest.toml`); `[[pack]]` solo con licencias de `REDISTRIBUTABLE_LICENSES` (CC0/CC-BY) y `validate()` que lo impone; `[[hub]]` con `HubKind::Plugin` solo enlace (`license`, `requires_account`, `formats`, `os`). Hoy hay 18 plugins en el hub (Vital, Surge XT, Dexed, OB-Xd, Tyrell N6, LABS, Valhalla Supermassive, TAL-NoiseMaker, Kilohearts Essentials, Odin 2, Decent Sampler, sforzando, sfizz, MFreeFXBundle, TDR Nova, Airwindows, Dragonfly, Cardinal), todos link-only. | Un tipo `[[plugin]]` con URLs por plataforma, hash por artefacto, `installer`, recibos. Mantener la regla legal en `validate()`: lo no redistribuible sigue siendo descarga directa del fabricante, no espejo. |
| `xtask/src/soundbank.rs` (1056 líneas) | `cargo xtask soundbank fetch`: descarga, calcula sha256 y **lo escribe en el manifiesto** (`set_pack_field`); comprueba mismatch; extracción zip/tar; `write_license`/`write_source` (`SOURCE.txt` con URL, sha256 y licencia). | Un `cargo xtask plugins pin|verify` análogo: resuelve URLs (incluida la API de GitHub para tags rodantes como `DAWPlugin`), calcula hashes y tamaños, y falla en CI si una URL fija ya no responde o el hash cambió. |
| `crates/ai/src/models.rs` (830 líneas) | `ModelManager::ensure`, reanudación con `<target>.part` y cabecera `Range`, sha256 y tamaño, `installed.json` por modelo, `accepted.json` y `accept_license` con `notice` obligatoria antes de bajar. | Reutilizar la **aceptación de aviso** y la **reanudación** (probablemente extraer un `download.rs` común en `crates/library` y que `ai` lo use). |
| `crates/host` | `default_search_paths` ya incluye las carpetas por usuario: macOS `~/Library/Audio/Plug-Ins/{VST3,CLAP}`; Windows `%CommonProgramFiles%\{VST3,CLAP}` **y** `%LOCALAPPDATA%\Programs\Common\{VST3,CLAP}`; además `CLAP_PATH`. `Scanner` (caché por `FileKey`, escaneo fuera de proceso con `scan_out_of_process`, lista de bloqueo, `add_search_path`) y `SandboxedPlugin` (host aislado). | Una llamada de "re-escanear solo estas rutas" tras instalar. |
| Memoria del proyecto | "Odin needs ad-hoc sign" y "CLAP cookies" en pruebas con plugins reales instalados en `~/Library/Audio/Plug-Ins`. | Confirma que macOS puede exigir firma ad-hoc. |

Estándares de rutas verificados [V]: CLAP define `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP`, `/Library/Audio/Plug-Ins/CLAP`, `~/Library/Audio/Plug-Ins/CLAP` y `CLAP_PATH` (https://github.com/free-audio/clap/blob/main/include/clap/entry.h); VST3 define `%LOCALAPPDATA%\Programs\Common\VST3` (usuario, prioridad 1; "mainly used for development use case"), `Program Files\Common Files\VST3` (global) y en macOS `~/Library/Audio/Plug-ins/VST3` y `/Library/Audio/Plug-ins/VST3` (https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/Locations+Format/Plugin+Locations.html).

---

## 5. Diseño del instalador en un clic (Parte C)

### 5.1 Principios

1. **Por usuario, sin elevación.** Nunca `sudo`, nunca UAC.
2. **Nunca ejecutar instaladores de terceros.** Se abren los contenedores y se copian solo los bundles `.vst3`/`.clap` (más presets y licencias).
3. **Hash fijado y host permitido o no se instala.** El manifiesto lo firma el equipo de AI-Track; el hash dice "este fichero es el que revisamos".
4. **Lo no redistribuible nunca pasa por nuestros servidores.** Descarga directa del fabricante iniciada por el usuario.
5. **El motor sigue funcionando sin red ni terceros.**

### 5.2 Manifiesto (propuesta)

Nuevo `content/plugins/manifest.toml`, compilado con `include_str!` como línea base, más un índice remoto opcional firmado (ed25519) para corregir URLs sin lanzar versión. Mismas convenciones que `hub.rs` (serde, `validate()`).

```toml
[meta]
version = 1
updated = "2026-11-15"

[[plugin]]
id            = "tdr-kotelnikov"
name          = "TDR Kotelnikov"
vendor        = "Tokyo Dawn Labs"
version       = "1.6.7"                    # versión que fijan los hashes
category      = "dynamics"                 # dynamics|eq|saturation|reverb|delay|modulation|glitch|vocal|utility|synth|sampler
tags          = ["compressor", "glue", "bus"]
homepage      = "https://www.tokyodawn.net/tdr-kotelnikov/"
license       = "Proprietary-Freeware"     # SPDX o etiqueta propia
license_url   = ""                         # vacío = sin EULA publicada
license_notice = "Gratis. Se descarga del servidor del fabricante; AI-Track no lo redistribuye."
lane          = "direct"                   # bundle | mirror | direct | link
redistributable = false
requires_account = false
requires_admin   = false
formats       = ["vst3"]
bundle_ids    = ["TDR Kotelnikov"]         # nombre del bundle esperado, para detectar "instalado" y desinstalar
allowed_hosts = ["www.tokyodawn.net"]

[[plugin.artifact]]
os      = "macos"
arch    = "universal"                      # universal|arm64|x86_64
url     = "https://www.tokyodawn.net/labs/Kotelnikov/1.6.7/TDR Kotelnikov.zip"
sha256  = "<64 hex, obligatorio>"
size    = 0                                # bytes exactos, tope de descarga
container = "zip"                          # zip|tar_gz|dmg|pkg|msi|exe_portable_zip
install  = [ { from = "**/*.vst3", to = "vst3" } ]   # pasos declarativos (ver 5.5)
signer   = { team_id = "XXXXXXXXXX" }      # macOS: Team ID esperado; Windows: { subject = "..." }; opcional si no firmado

[[plugin.artifact]]
os = "windows"
arch = "x86_64"
url = "https://www.tokyodawn.net/labs/Kotelnikov/1.6.7/TDR Kotelnikov (no installer).zip"
sha256 = "<...>"
container = "zip"
install = [ { from = "**/*.vst3", to = "vst3" } ]
```

Ejemplos de variantes:
- **Tag rodante (Airwindows):** `resolve = { github = "baconpaul/airwin2rack", tag = "DAWPlugin", asset_glob = "airwindows-consolidated-macOS-*.dmg" }`; la app resuelve el asset por API y compara con `artifact_sha256sum.txt`; el hash *fijado* se actualiza con `cargo xtask plugins pin` y se publica por el índice firmado. Si el hash del asset resuelto no coincide con el manifiesto, no se instala.
- **Enlace guiado (L3):** `lane = "link"`, sin `[[plugin.artifact]]`; campos `download_page`, `account_hint` ("Kilohearts Installer + cuenta gratuita") y `detect` (nombres de bundle para marcar "Instalado" tras el re-escaneo).
- **Mirror (L1):** solo si `redistributable = true` y `license` está en una lista de permitidas (GPL-3.0, AGPL-3.0, BSD-2/3, MIT, Apache-2.0) o es `"u-he-EULA"`; exige `source_url` (fuente exacta para cumplir GPL) y `license_file`.

Campos de `validate()` (extendiendo el de `hub.rs`): `lane` coherente (`direct`/`mirror` requieren artefactos con sha256 de 64 hex y `url` `https://`; el host de `url` debe estar en `allowed_hosts`; `mirror` exige `redistributable` y `source_url`; `bundle`/`mirror` de licencia no permitida = error); sin duplicados de `id`; `size` obligatorio.

### 5.3 Rutas de instalación

| SO | Destino por defecto (sin admin) | Notas |
|---|---|---|
| macOS | `~/Library/Audio/Plug-Ins/VST3/` y `~/Library/Audio/Plug-Ins/CLAP/` | Estándar [V] y ya escaneado por `default_search_paths`. |
| Windows VST3 | `%LOCALAPPDATA%\Programs\Common\VST3\` | Estándar de usuario de VST3 [V]. Ya escaneado por nosotros. **Otros DAWs** pueden no mirar esa carpeta (el propio estándar dice que es sobre todo para desarrollo). |
| Windows CLAP | `%LOCALAPPDATA%\Programs\Common\CLAP\` | Estándar de CLAP [V]. |
| Alternativa privada | `%LOCALAPPDATA%\AI-Track\plugins\{vst3,clap}` / `~/Library/Application Support/AI-Track/plugins/...` + `Scanner::add_search_path` | Aísla y facilita desinstalar, pero los plugins no los ven otros DAWs. Ofrecer como opción en Ajustes; **el valor por defecto es la carpeta estándar por usuario** (interoperabilidad). |
| Carpeta de sistema | `%COMMONPROGRAMFILES%\VST3`, `/Library/Audio/Plug-Ins/...` | No se escribe nunca desde la app. Si el usuario ya tiene el plugin allí, se detecta ("Instalado (externo)") y se oculta el botón Instalar. |

Plugins que **solo** funcionan instalados por su propio instalador (licencia, preferencias, activación, componentes en ProgramData/Application Support, p. ej. Kilohearts, Slate, NI) son siempre L3: no se intenta copiar a mano.

### 5.4 Contenedores (sin ejecutar nada)

| Contenedor | Cómo abrirlo | Observaciones |
|---|---|---|
| `zip` | `zip` (ya en `install.rs`) **añadiendo**: restaurar modo Unix (`unix_mode()`), crear symlinks dentro de la raíz de extracción, rechazar enlaces que apunten fuera, límites (p. ej. <= 20 000 ficheros, ratio <= 100:1, <= 2 GiB descomprimidos). | Mayoría de TDR, TAL, u-he, Dexed, Surge `pluginsonly`, Airwindows. |
| `tar.gz`/`tar.xz`/`7z` | ya soportados | |
| `dmg` (macOS) | `hdiutil attach -readonly -nobrowse -noautoopen -noverify -mountrandom <tmp> <dmg>`, copiar los `.vst3`/`.clap` (con `ditto`, preserva permisos y symlinks) y `hdiutil detach`. Montar un dmg no ejecuta código. | Surge, Dexed, ZL, Youlean, Airwindows. En CI/tests no hay GUI: usar `-nobrowse`. |
| `pkg` (macOS) | **No ejecutar `installer`.** `pkgutil --expand-full <pkg> <dir>` y copiar los bundles del `Payload` (ignorar `scripts/`). Si el pkg trae `postinstall` esencial -> se marca L3. | Odin 2, sfizz, Cardinal, OB-Xd, X-Audio. |
| `msi` (Windows) | Extraer sin instalar con `msiexec /a <msi> /qn TARGETDIR=<tmp>` (imagen administrativa; no requiere elevación si el destino es de usuario) o una lectura directa de tablas CAB. | ZL Audio. Si el msi tiene acciones personalizadas imprescindibles, L3. |
| `exe` instalador (NSIS/Inno) | **No se ejecuta.** Preferir siempre el zip "no installer"/portable que ofrece el fabricante (TDR, Surge `pluginsonly`, Dexed `.zip`, Airwindows `.zip`, Decent Sampler). Si solo hay `.exe`, el plugin pasa a L3 (guiado). | Evita ejecutar binarios arbitrarios con privilegios. |

Todo se hace en `.work/` dentro del directorio de datos, con comprobación de hash **antes** de abrir el contenedor.

### 5.5 Pasos de instalación (declarativos)

`install = [ { from = "<glob dentro del contenedor>", to = "vst3|clap|presets|docs", rename = "..." } ]`. Los glob solo pueden seleccionar `*.vst3`, `*.clap` (bundles completos), carpetas de presets y ficheros de licencia; nada ejecutable más. Siempre se copian `LICENSE`/`EULA` a `<destino>/.aitrack/<id>/` junto al recibo (para GPL, la oferta de fuente: `SOURCE.txt` con `source_url`).

### 5.6 Gatekeeper, cuarentena y firma (macOS)

[S] salvo indicación. Datos de plataforma conocidos, no verificados en esta sesión:
- Los ficheros que descarga nuestra app con `ureq` **no reciben** el atributo `com.apple.quarantine` (lo ponen los clientes que lo piden). Por tanto, en principio, no hay prompt de Gatekeeper. Si el usuario copia plugins desde el navegador, sí lo habrá.
- En Apple Silicon todo código arm64 debe llevar al menos firma ad-hoc o el kernel lo mata. Algunos builds de proyectos open source no firman (Cardinal lo dice expresamente [V]; nuestra memoria del proyecto: Odin necesita firma ad-hoc). Paso post-extracción: `codesign --verify --deep --strict`; si no está firmado, `codesign --force --deep -s -` (ad-hoc) y registrarlo en el recibo.
- Si el binario firmado por otro Team ID se carga dentro del proceso de AI-Track, con **hardened runtime** hace falta el entitlement `com.apple.security.cs.disable-library-validation` en nuestro ejecutable (y en el host de sandbox). Comprobarlo en el empaquetado.
- Verificar Team ID contra `signer.team_id` del manifiesto con `codesign -dv --verbose=4`. Solo tras verificar el sha256 fijado se permite `xattr -dr com.apple.quarantine <bundle>` (nunca de forma ciega).
- `spctl --assess --type execute` sirve como señal informativa, no como bloqueo (muchos freeware no están notarizados).

### 5.7 Windows

- Extraer solo a `%LOCALAPPDATA%\Programs\Common\{VST3,CLAP}`.
- Comprobar Authenticode con `WinVerifyTrust` (API) si el fabricante firma; si no, se confía solo en el sha256 fijado y se avisa en la tarjeta.
- Marca "Mark of the Web": no se añade a lo que descargamos. Si el zip ya trae `Zone.Identifier`, no propagarlo es decisión de política (recomendado: quitarlo solo tras verificar sha256).
- DLL de dependencias (VC++ runtime): si falta, el escaneo lo notificará como fallo de carga; indicarlo en la ficha ("requiere Visual C++ Redistributable"). Instalarlo queda fuera (requiere admin).

### 5.8 Descarga

Extender `install::download` (o extraer un módulo común con `ai::models`):
- Solo `https://`; el host de la URL **y de cada redirección** debe estar en `allowed_hosts` (para GitHub: `github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`; para S3/CDN, el host concreto).
- Reanudación con `.part` + `Range` (como `models.rs`), tope = `size` del manifiesto (abortar si se excede), tiempo de lectura máximo.
- Hash obligatorio; si no coincide: borrar, no instalar, mostrar "el fabricante ha publicado una versión nueva; abre su página" y registrar para el mantenedor.
- User-Agent genérico `AI-Track`, sin datos del usuario.

### 5.9 Flujo, recibos, re-escaneo

1. Usuario pulsa **Instalar** -> si hay `license_notice`/EULA: diálogo con el texto y botón de aceptar (reusa `accept_license` de `models.rs`).
2. `PluginInstallJob` (hermano de `InstallJob`): descarga -> hash -> abrir contenedor en `.work/<id>/raw` -> aplicar `install` a un *staging* -> verificar firma/codesign -> mover a la carpeta destino con renombrado atómico (si existe versión previa, mover a `.old` para revertir).
3. **Recibo** `<datos>/plugins/installed/<id>.json`: `id`, `version`, `lane`, `source url`, `sha256` del artefacto, lista exacta de rutas instaladas con hash de árbol de cada bundle, fecha, firma observada, `adhoc_signed`.
4. **Re-escaneo:** `Scanner::add_search_path` ya cubre las carpetas; añadir `rescan_paths(&[PathBuf])` que escanea solo los bundles nuevos fuera de proceso (`scan_out_of_process`) y actualiza la caché. Si falla o se cuelga, entra en la lista de bloqueo y la tarjeta muestra "No se pudo cargar" con el motivo y botón Desinstalar.
5. Evento a la UI: la lista de dispositivos se refresca con el nuevo plugin.
6. **Desinstalar:** borrar solo las rutas del recibo (y comprobar que están dentro de las carpetas de plugin de usuario; nunca rutas fuera). Si hay instancias abiertas en el proyecto, avisar ("el plugin quedará ausente en estos proyectos"). En Windows, un `.vst3` cargado está bloqueado: cerrar instancias o diferir al próximo arranque.
7. **Actualizar:** la tarjeta compara `manifest.version` con el recibo; "Actualizar" repite el flujo con rollback. Las URLs con tag rodante se comparan por hash del asset.
8. **Salud del catálogo (CI):** `cargo xtask plugins verify` (semanal) hace HEAD/descarga parcial de cada URL, compara hash/tamaño y abre un aviso si cambia; así se detecta que el fabricante reemplazó el fichero antes de que lo vea un usuario.

### 5.10 UI: pestaña "Plugins gratuitos"

En el Content Hub, junto a Sonidos:
- **Tarjeta** por plugin: nombre, fabricante, categoría, formato (VST3/CLAP), plataforma, tamaño, **insignia de carril** ("Incluido open source", "Descarga del fabricante", "Cuenta requerida"), licencia, y estado: `Instalar` / `Instalando 45 %` (con cancelar) / `Instalado` (v1.6.7) / `Actualizar a v1.6.8` / `Instalado (externo)` / `Abrir página oficial` (L3) / `Fallo: ...`.
- Filtros por categoría (dinámica, EQ, saturación, reverb, delay, modulación, glitch/stutter, voz, utilidad, sintes, samplers) y carril; buscador.
- **Paquetes recomendados** de un clic con tamaño total: "Esencial gratuito" (Surge XT, Dexed, TDR Kotelnikov, TDR Nova, TAL-Reverb-4, TAL-Chorus-LX, Youlean), "Stutter y glitch" (FAULTLINE y Glitch² vía enlace/descarga), etc.
- Detalle: licencia y aviso, qué se instalará y dónde, botón "Abrir carpeta", "Ver recibo", "Desinstalar".
- Estilo según CLAUDE.md: colores del tema, tiempos de `motion.rs`, iconos de `icons.rs`, espaciado en múltiplos de 4 px.

### 5.11 Cumplimiento de licencias al repartir binarios (L1)

- **GPL-3 junto a una app MIT:** un binario GPL separado que se distribuye desde nuestra infraestructura es, a la luz de las FAQ de la GPL, "mera agregación" si no se enlaza ni se mezcla con código nuestro [S, no es asesoría legal]. Obligaciones por binario GPL que *nosotros* alojemos o empaquetemos: texto de la licencia, avisos de copyright, **ofrecer el código fuente exacto** (enlace al tag/commit; o oferta escrita), no imponer restricciones adicionales. Instalarlo como componente **opcional y separado**, con su `LICENSE` y su `source_url`, y anotarlo en `NOTICE`.
- **AGPL (ZL Audio):** no entra por `deny.toml` como dependencia; como binario independiente descargado a petición del usuario, desde el release del autor, no hay distribución nuestra. **No empaquetar ni alojar en espejo**; solo descarga directa.
- **u-he:** si alguna vez se empaqueta, hay que enviar el EULA con el software, que sea gratis, sin modificar y sin quitar avisos; recomendable pedir confirmación por escrito a u-he para el caso "dentro del instalador de otro producto" (el EULA no lo trata expresamente) [V].
- **Vital:** presets del plan gratuito no redistribuibles; los binarios no son GPL. Solo enlace.
- **Airwindows:** el código es MIT, pero el binario DAW trae dependencias GPL3: tratarlo como GPL-3.
- **Marcas:** no usar el logo ni el nombre de fabricantes en la UI más allá de su nombre en texto.

### 5.12 Riesgos de seguridad y mitigaciones

| Riesgo | Mitigación |
|---|---|
| Un plugin de terceros es código nativo con todos los privilegios del usuario, cargado en nuestro proceso. | Hosting en proceso aislado (`SandboxedPlugin` del crate `host`) como valor por defecto para plugins de terceros; escaneo fuera de proceso con timeout y lista de bloqueo; aviso en el primer uso; límite de privilegios del proceso sandbox. |
| Descarga sustituida o servidor del fabricante comprometido | sha256 fijado por artefacto (obligatorio), tamaño máximo, HTTPS con verificación de plataforma (`ureq` con `platform-verifier`), hosts permitidos incluidos los de redirección. Un cambio en el servidor hace fallar el hash (fail closed). |
| Manifiesto manipulado (MITM del índice remoto) | Índice remoto firmado con ed25519; clave pública compilada; línea base compilada en el binario; lista de revocación (`revoked = ["id@version"]`) que desinstala o bloquea. |
| El hash fijado solo prueba "es el fichero revisado", no que sea inocuo | Revisión humana al fijar (firma/Team ID esperados en `signer`, escaneo de antivirus en CI, comparación con la release oficial, repro del hash por dos personas). |
| Zip slip, symlinks, zip bombs | `enclosed_name` ya usado; añadir verificación de symlinks, límites de nº de ficheros, ratio y tamaño total; extracción en `.work` y mover solo bundles permitidos. |
| Ejecución de instaladores o scripts `postinstall` | Política: nunca. Solo contenedores abiertos con herramientas de extracción; `pkg` se expande, no se instala. |
| Elevación de privilegios | Nunca se pide admin; las carpetas de sistema son de solo lectura para la app. |
| Plugins que activan licencias o telemetría propios | Mostrar `requires_account`/`license_notice` en la tarjeta; L3 para todo lo que exija activación. |
| Colisiones de nombre / sobrescribir un plugin que el usuario ya tenía | Detectar el bundle existente (por nombre y `bundle_ids`); marcar "Instalado (externo)" y no sobrescribir; recibos solo de lo que instalamos. |
| Plugin que cuelga/cierra el DAW | Sandbox + bloqueo automático tras crash; botón "Desinstalar" siempre visible en la tarjeta con fallo. |
| Dependencia de URLs de terceros que cambian | CI semanal de verificación, ficha "Fabricante cambió la versión: abre su página" como degradación, y posibilidad de tag rodante con resolución por API. |
| Responsabilidad legal por deep-linking | Solo ofrecer descarga directa cuando la URL es pública y la inicia el usuario; para Tokyo Dawn, cuyos términos prohíben "distribute" su material web, pedir confirmación por escrito antes de lanzar. |

---

## 6. Recomendación y orden de trabajo

**Plugins imprescindibles (15):** Surge XT, Dexed, Odin 2, Zebralette 3, TDR Nova, TDR Kotelnikov, ZL Equalizer 2 (AGPL, bajo demanda), TAL-Reverb-4, TAL-Chorus-LX, TAL-Vocoder, X-Audio FAULTLINE, illformed Glitch², Youlean Loudness Meter 2, LoudMax, Valhalla Supermassive (pendiente de confirmar EULA/formato).

**Fases:**
1. **Hub nuevo con las 63 entradas** como `[[plugin]]` con `lane = "link"` o `"direct"` sin automatizar (solo enlaces y estado "instalado" por escaneo). Ya aporta valor y no requiere descargas.
2. `PluginInstallJob` + zip con permisos/symlinks + hosts permitidos + hash obligatorio + recibo, para los **zip portables** (TDR, TAL, u-he, Dexed, Surge `pluginsonly`, Airwindows).
3. dmg/pkg/msi, firma ad-hoc/Team ID, entitlements.
4. Índice remoto firmado, `xtask plugins pin|verify`, CI.
5. Dispositivos propios para los huecos: multibanda dinámica estilo OTT, stutter/beat-repeat/half-time/tape-stop, pitch/formante y vocoder.

## 7. Pendientes y dudas abiertas

- OTT: confirmar con el instalador real (inspección `pkgutil --expand-full`) si Mac incluye `.vst3` y si es arm64.
- Valhalla: leer EULA y formatos oficiales (la web devolvió 403); confirmar que FreqEcho y Space Modulator siguen gratis.
- Dexed 1.1.0, Dragonfly 3.2.10, Odin 2 y Cardinal: formatos reales y Apple Silicon.
- Klanghelm, Analog Obsession (soporte macOS reciente), Glitchmachines y Cymatics: formatos y licencia.
- TDR Molotok: confirmar que es gratis.
- Waves Free Pack: lista exacta y términos (waves.com devolvió 404).
- Todos los EULA citados como "NC": leer el texto íntegro; para cualquier idea de empaquetado de freeware propietario, pedir **permiso escrito** al fabricante.
- Identificar la marca "aterboys" con el dueño.
