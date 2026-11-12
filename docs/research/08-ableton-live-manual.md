# 08 · Manual de Ableton Live y control bar → mejoras para AI-Track

> Fecha de la investigación: 2026-11-12. Manual de referencia: **Ableton Live 12** (versión web en `https://www.ableton.com/en/manual/welcome-to-live/`, que redirige a `/en/live-manual/12/…`).
> Objetivo: (1) entender cómo está construido el manual web de Ableton y qué copiar de él, (2) tener el esqueleto completo del manual, (3) describir el **control bar** de Live elemento por elemento y (4) proponer cambios concretos al titlebar de AI-Track, que no convence al usuario.
> Nota de fiabilidad: el texto del manual y las URL de las capturas se leyeron directamente del HTML de las páginas (marcado **[P]**). **No se pudo descargar ni ver el PNG del control bar** (el host de imágenes no respondió desde esta sesión), así que lo que se dice sobre *tamaños relativos, proporciones y estilo visual* viene de conocimiento consolidado del producto y se marca **[M]**. Conviene validarlo contra una captura propia de Live 12 antes de cerrar el diseño.

---

## Índice

1. [Cómo funciona el manual web de Ableton](#1-cómo-funciona-el-manual-web-de-ableton)
2. [Esqueleto completo del manual (Live 12)](#2-esqueleto-completo-del-manual-live-12)
3. [El control bar de Live en detalle](#3-el-control-bar-de-live-en-detalle)
4. [Comparativa: control bar de Live frente al topbar de AI-Track](#4-comparativa-control-bar-de-live-frente-al-topbar-de-ai-track)
5. [Recomendaciones para AI-Track](#5-recomendaciones-para-ai-track)
6. [Fuentes](#6-fuentes)

---

## 1. Cómo funciona el manual web de Ableton

### 1.a Estructura de navegación [P]

| Elemento | Qué hace | Detalle observado |
|---|---|---|
| Cabecera global | Enlaces a Live, Push, Move, Note, Link, Shop, Packs, Help y "More"; botón "Try Live 12 for free" | Es la cabecera del sitio, no del manual |
| **Selector de versión** | Alterna entre "Version 12" y "Version 11" | La 11 vive en `/en/live-manual/11/<capítulo>/`; la 12 en `/en/live-manual/12/<capítulo>/`. La URL conserva el slug del capítulo, así que se salta de versión sin perder el sitio |
| **Selector de idioma** | English, Deutsch, français, 日本語, 简体中文, español, italiano | Cada página declara `<link rel="alternate" hreflang>` hacia `/de/live-manual/12/…`, `/ja/…`, etc. |
| **Descarga en PDF** | Enlace "Download PDF" al manual completo | El manual existe también como un único documento imprimible |
| **Índice (TOC)** | Lista numerada y anidada de los 43 capítulos y sus secciones, antes del cuerpo | Cada entrada es un enlace a un ancla (`#the-control-bar`) de la misma página o de otro capítulo |
| **Buscador** | No aparece ninguna barra de búsqueda propia del manual en el HTML descargado | La búsqueda va por el buscador general del sitio o por Google; es una carencia del manual web frente a la ayuda moderna (ver lecciones) |
| Pie | Newsletter, redes, comunidad, aviso legal | Sin relación con el contenido |

> Nota: la ruta antigua `/en/manual/…` que cita el encargo redirige a `/en/live-manual/12/…`; son el mismo contenido.

### 1.b Formato de cada capítulo [P]

- **Una página HTML por capítulo**, con todas sus secciones seguidas (la página de *Live Concepts* pesa ~300 KB de HTML). No hay paginación interna.
- **Numeración jerárquica en el propio encabezado**: `3.1 The Control Bar`, `4.2.1 Saving Search Results as Custom Labels`. Cada `<h2>` lleva `data-number="3.1"` y un `id` estable y legible (`id="the-control-bar"`). El número sirve para citar ("véase 3.1") y el id para enlazar.
- **Cuerpo en prosa corta**: un párrafo introductorio que dice *para qué sirve* el elemento, luego una lista que enumera sus partes con el patrón `Nombre de la parte - qué contiene`. Ejemplo literal del control bar: "The Control Bar is grouped into nine sections: Browser Options - …, Tempo Settings and Metronome - …".
- **Atajos de teclado inline**, siempre con las dos plataformas: "Alt 1 (Win) / Option 1 (Mac)".
- **Capítulos de referencia separados de los conceptuales**: los de dispositivos (29-31, 33) son catálogos; los de flujo (6-28) explican tareas.

### 1.c Uso de capturas de pantalla [P]

- Las imágenes son `<figure class="image-container">` con `<figcaption>` corto en inglés, p. ej. "The Control Bar.", "The Status Bar.", "The Browser.".
- Se sirven desde un CDN de imágenes con parámetros de tamaño (`https://ableton-production.imgix.net/live-manual/12/<Nombre>L12.png?auto=&w=320` con variantes responsivas `w=320 320w, …`).
- **Convención de nombres de archivo**: `<Concepto>L12.png` (`ControlBarL12.png`, `StatusBarL12.png`, `TempoControlL12.png`). El sufijo `L12` marca la versión de Live, de modo que cada versión del manual lleva su propio juego de capturas sin colisiones.
- En la página *Live Concepts* hay 41 imágenes distintas. Las capturas son **recortes del componente concreto** (un solo control bar, una sola barra de estado), no pantallazos completos de la app.
- En el HTML extraído **no hay anotaciones sobre las capturas** (sin flechas ni números en el marcado). Si la imagen lleva anotaciones, van quemadas en el PNG; el texto no depende de ellas: la explicación elemento por elemento está en la lista escrita. Es una decisión útil: la captura ilustra, el texto informa y es accesible/traducible.
- Las imágenes del encabezado y pie del sitio (`ableton-wordmark`, `ableton-hallmark`) son SVG; los textos alternativos de las capturas del manual no están rellenos en el marcado observado (carencia de accesibilidad).

### 1.d Enlaces cruzados [P]

- Dentro del texto, cada término técnico se enlaza a su sección ("controles para activar **Link**, fijar el tempo, personalizar el **metronome**, activar **Tempo Follower**"): el control bar enlaza a Link, al capítulo de Metronome, a Capture MIDI, a Automation, etc.
- Los enlaces son relativos y anclados (`/en/live-manual/12/<capítulo>/#<ancla>`), así que sobreviven a reordenar el sitio mientras el id no cambie.
- *Live Concepts* actúa de **hub**: cada concepto (3.1-3.21) resume y manda al capítulo profundo (Browser → cap. 4, Mezclador → cap. 18, Routing → cap. 17…).

### 1.e Lecciones de documentación y UX para AI-Track

| Lección | Cómo aplicarla en AI-Track |
|---|---|
| Un capítulo-hub de conceptos antes de los capítulos profundos | `docs/` (o la ayuda in-app) con una página "Conceptos": barra de control, barra de estado, browser, vistas, pistas, comandos |
| Descripción de un componente = párrafo de propósito + lista `Parte - función` | Usar esa plantilla para documentar cada zona del titlebar (y para sus tooltips largos) |
| Ids de ancla estables y numeración citable | En la futura ayuda in-app, cada pantalla con id fijo; el asistente de IA puede enlazar con `ayuda://control-bar#transporte` |
| Capturas que son recortes del componente, nombradas por concepto y versión | Generar las capturas de la documentación automáticamente desde la UI (modo de captura por vista) en vez de pantallazos manuales |
| Texto completo junto a la imagen (la imagen no es la única fuente) | Mantener los tooltips y el texto de ayuda como fuente de verdad; la captura solo ilustra |
| Selector de versión conservando el slug | Si hay web de docs, `/docs/<version>/<slug>` |
| Atajos con ambas plataformas | Ya hacemos `Ctrl+…` en tooltips; mostrar el atajo según plataforma (`Cmd` en macOS) |
| Ausencia de buscador en el manual | Oportunidad: buscador en la ayuda in-app, y el asistente IA que responda con el manual como base (patrón "Gopher" de FL, ver informe 03 §2.d) |
| `alt` vacío en capturas | Rellenar siempre `alt` |

---

## 2. Esqueleto completo del manual (Live 12)

43 capítulos [P] (`https://www.ableton.com/en/live-manual/12/<slug>/`).

### 2.a Fundamentos (1-5)

| # | Capítulo | Qué cubre |
|---|---|---|
| 1 | Welcome to Live | Introducción, qué es Live, versiones y licencias |
| 2 | First Steps | Instalación, configuración de audio/MIDI, primer Set |
| 3 | Live Concepts | **Hub**: control bar (3.1), barra de estado (3.2), browser, similitud sonora, Sets, Arrangement/Session, pistas, audio y MIDI, clips, dispositivos, vista de clip/dispositivo, escalas (3.13), mezclador, racks, routing, grabación, automatización, MIDI/teclas remotas, guardado y exportación |
| 4 | Working with the Browser | Panel de contenido, buscador, historial, filtros y etiquetas, colecciones, librería, lugares (Packs, Splice, Cloud, User Library, Current Project), previsualización |
| 5 | Managing Files and Sets | Archivos de samples y MIDI, Sets y Proyectos, historial de deshacer del Set, fusionar Sets, plantillas, reparar archivos perdidos, recopilar externos |

### 2.b Vistas y edición (6-15)

| # | Capítulo | Qué cubre |
|---|---|---|
| 6 | Arrangement View | Línea de tiempo lineal, navegación, edición de clips, loop y punch |
| 7 | Session View | Matriz de clips y escenas |
| 8 | Clip View | Propiedades del clip: launch, loop, notas, envolventes |
| 9 | Audio Clips, Tempo, and Warping | Warp markers, algoritmos, tempo del Set |
| 10 | Editing MIDI | Editor de notas, cuantización, escala |
| 11 | MIDI Tools | Generators y Transformations de Live 12 |
| 12 | Editing MPE | Edición de expresión por nota |
| 13 | Converting Audio to MIDI | Melodía, armonía y percusión desde audio |
| 14 | Using Grooves | Pool de grooves |
| 15 | Using Tuning Systems | Afinaciones no temperadas |

### 2.c Reproducción, mezcla y grabación (16-22)

| # | Capítulo | Qué cubre |
|---|---|---|
| 16 | Launching Clips | Cuantización de lanzamiento, follow actions |
| 17 | Routing and I/O | Entradas, salidas, envíos, sidechain |
| 18 | Mixing | Mezclador, envíos, retornos, panorama |
| 19 | Recording New Clips | Grabar audio/MIDI, overdub, Capture MIDI |
| 20 | Bounce to Audio | Congelar y renderizar |
| 21 | Comping | Takes y compuesto |
| 22 | Stem Separation | Separación local de stems (IA) |

### 2.d Dispositivos, automatización y extensiones (23-33)

| # | Capítulo | Qué cubre |
|---|---|---|
| 23 | Working with Instruments and Effects | Cadenas de dispositivos, presets |
| 24 | Using Plug-Ins | VST/AU, ventanas de plugin |
| 25 | Instrument, Drum and Effect Racks | Racks, macros, cadenas |
| 26 | Automation | Envolventes de automatización, modos de grabación |
| 27 | Clip Envelopes | Envolventes ligadas al clip |
| 28 | Working with Video | Vídeo sincronizado |
| 29-31 | Live Audio Effect / MIDI Effect / Instrument Reference | Catálogo de dispositivos propios |
| 32-33 | Max for Live / Max for Live Devices | Extensión y dispositivos Max |

### 2.e Control externo, sincronía y anexos (34-43)

| # | Capítulo | Qué cubre |
|---|---|---|
| 34 | MIDI and Key Remote Control | Mapeo de controladores y teclado |
| 35-36 | Using Push 1 / Push 2 | Hardware Push |
| 37 | Synchronizing with Link, Tempo Follower, and MIDI | Ableton Link, seguidor de tempo, reloj MIDI |
| 38 | Computer Audio Resources and Strategies | Rendimiento, CPU, buffers |
| 39-40 | Audio Fact Sheet / MIDI Fact Sheet | Datos técnicos |
| 41 | Accessibility and Keyboard Navigation | Navegación por teclado |
| 42 | Live Keyboard Shortcuts | Tabla completa de atajos |
| 43 | Credits | Créditos |

**Observación de estructura**: el patrón es *conceptos → vistas → tareas → catálogo → hardware → anexos*. Es un buen orden para la ayuda de AI-Track: la mayoría de usuarios nuevos solo necesitan los capítulos 1-3 y 42.

---

## 3. El control bar de Live en detalle

Fuente principal: sección **3.1 The Control Bar** de *Live Concepts* (`https://www.ableton.com/en/live-manual/12/live-concepts/#the-control-bar`) [P].

El manual lo define así: *"el sitio de los controles de transporte y tempo, y de otros controles útiles para personalizar la escala y los ajustes MIDI, vigilar el uso de CPU y alternar entre Session y Arrangement"*. Está agrupado en **nueve secciones**. El manual las lista en este orden, que coincide con el orden de izquierda a derecha de la barra (el texto no lo afirma literalmente; el orden de la lista y la disposición conocida de Live coinciden [M]).

### 3.a Diagrama (distribución aproximada) [M]

```
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [▤][⋯] │ Link  Tempo  Sig  Metro  TapFollow │ Key Scale │ Follow  Pos │ ▶ ■ ● │ OVR ARM RE● ⏺Cap  ⏺Sess │ Loop Punch │ Draw ⌨ Key MIDI SR CPU │ [Session|Arr] │
│  1      │ 2                                  │ 3         │ 4           │ 5     │ 6                        │ 7          │ 8                       │ 9             │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 Izquierda ───────────────────── transporte y tempo en el centro-izquierda ───────────────────────── derecha
```

La barra ocupa todo el ancho de la ventana y es **una sola fila baja (unos 32-40 px a escala 1x)** [M] encima de todo el contenido; Live usa la barra de título nativa del sistema aparte (no fusiona el control bar con los botones de ventana de Windows/macOS) [M].

### 3.b Elemento por elemento, de izquierda a derecha [P salvo lo marcado]

| # | Sección | Qué muestra / controla | Notas de agrupación y tamaño [M] |
|---|---|---|---|
| 1 | **Browser Options** | Interruptor *Show/Hide Browser* y el *Browser Config Menu* (expandir el browser a toda la altura; mostrar secciones *Tuning* y *Groove Pool*) | Extremo izquierdo. Dos botones pequeños y cuadrados. Atajo del browser: Ctrl+Alt+B (Win) / Cmd+Option+B (Mac) |
| 2 | **Tempo Settings and Metronome** | Activar **Link**; **campo de Tempo** (BPM); **compás** (time signature); ajustes del **metronome**; **Tempo Follower** on/off | Es el grupo más ancho y el que más se mira. El campo de tempo es el número más grande de la barra; el compás va pegado a su derecha como dos campos numéricos apilados o en línea |
| 3 | **Scale Settings** | Refleja la escala del clip seleccionado. Los cambios se aplican al clip/slot seleccionado y a los clips creados después. Permite activar/desactivar Scale Mode o fijar la misma escala a varios clips (`ScaleModeControlBarL12.png`) | Dos pequeños selectores (tónica y nombre de escala) más un interruptor. Es contextual: depende de la selección |
| 4 | **Follow and Arrangement Position** | Interruptor **Follow** (la vista sigue el cursor de reproducción) y **posición** actual del Arrangement, visible y editable | Campo de posición `compases.negras.semicorcheas`, ancho fijo; es el segundo "número grande" |
| 5 | **Transport Controls** | Iniciar/parar reproducción y comenzar la **grabación de Arrangement** | Tres botones agrupados juntos con tamaño mayor: Play, Stop y Record. Es el centro visual |
| 6 | **Automation and Capture MIDI** | Ajustes de **overdub MIDI**; **armar automatización**; **volver a habilitar automatización** de parámetros anulados (*Re-Enable Automation*); **Capture MIDI**; iniciar **grabación de Session** | Grupo de botones pequeños de estado. *Re-Enable Automation* se enciende en naranja solo cuando hay parámetros sobrescritos |
| 7 | **Arrangement Loop Settings** | Activar y configurar el **loop** de Arrangement y los puntos de **punch-in/punch-out** | Interruptor de loop más campos de inicio/longitud, más los interruptores de punch |
| 8 | **MIDI and CPU Settings** | **Draw Mode**; **Computer MIDI Keyboard**; modos **Key Map** y **MIDI Map**; **sample rate** del proyecto; **medidor de CPU** | Extremo derecho previo al selector de vista. Medidor de CPU pequeño con valor en % |
| 9 | **View Selector** | Alterna entre **Session** y **Arrangement** (`ArrangementAndSessionSelectorsL12.png`) | Pareja de botones en el extremo derecho. Atajos: Alt/Option 1 = Session, Alt/Option 2 = Arrangement |

Además, el manual menciona en otras secciones un **Campo de Tempo** (`TempoControlL12.png`) y un **selector de cuantización** (*Quantization Chooser*, cuantización global de lanzamiento) que forman parte de la barra [P].

### 3.c Qué hay en cada extremo y en el centro

- **Extremo izquierdo**: *solo* acceso al browser. No hay logo, nombre de la app, menú de archivo ni título del proyecto: el sistema operativo aporta el título de la ventana y el menú está en la barra de menús (macOS) o bajo la ventana (Windows).
- **Centro-izquierda**: tempo, compás, metrónomo, Link y escala. Es la información "musical" del proyecto.
- **Centro**: posición y transporte (Play/Stop/Record). El transporte no está literalmente centrado; está tras tempo y escala [M].
- **Centro-derecha**: grabación avanzada (overdub, automatización, Capture MIDI) y loop/punch.
- **Extremo derecho**: utilidades de sistema (CPU, sample rate, Key/MIDI map) y, al final, el **selector de vista** Session/Arrangement.
- **Barra de estado** (3.2, `StatusBarL12.png`): mensajes de error/actualización, y al editar notas muestra posición, tono, velocidad y probabilidad de la nota; con el cursor sobre un marcador de inserción muestra su posición exacta. Está **abajo**, no en la barra de control. Es lo que libera al control bar de texto informativo.

### 3.d Principios de diseño que se deducen

1. **Agrupación por tarea, no por tipo de widget**: nueve grupos con nombre, cada uno corresponde a una acción mental (preparar tempo → ver posición → tocar → grabar → hacer loop → ajustes del sistema → cambiar de vista).
2. **Todo lo que no es de uso constante está oculto o reducido**: sample rate, Key/MIDI map y Draw Mode son pequeños; el tempo y la posición, grandes.
3. **El control bar no tiene identidad de marca**: ni logo ni título; la marca no ocupa espacio de trabajo.
4. **Contextualidad**: el bloque de escala refleja la selección; Re-Enable Automation solo destaca cuando hace falta.
5. **El cambio de vista está al final de la fila**, separado de los controles musicales, y tiene atajo de teclado.
6. **Los mensajes viven en la barra de estado**, no en la de control.

### 3.e Capturas del manual que ilustran el control bar y su entorno

Todas con prefijo `https://ableton-production.imgix.net/live-manual/12/` [P]. No se pudieron abrir los PNG; la descripción sale del pie de foto y del contexto [P] / [M].

| Captura (archivo) | Pie en el manual | Qué ilustra | Sección |
|---|---|---|---|
| `ControlBarL12.png` | "The Control Bar." | La barra completa con las nueve secciones | 3.1 |
| `ScaleModeControlBarL12.png` | "Scale Options in the Control Bar." | Controles de escala del control bar | 3.13 Scale Awareness |
| `ScaleModeClipViewL12.png` | (escala en la vista de clip) | Los mismos ajustes en el *Clip View* | 3.13 |
| `TempoControlL12.png` | "The Control Bar's Tempo Field." | Campo de tempo ampliado | 3.8-3.9 (warping) |
| `ArrangementAndSessionSelectorsL12.png` | "The Arrangement and Session View Controls." | Selector de vista | 3.6 |
| `ArrangementRecordButtonL12.png` | (botón de grabación de Arrangement) | Botón Record del transporte | 3.17 |
| `BackToArrangementButtonArrangementL12.png`, `…SessionL12.png`, `…TrackL12.png` | (Back to Arrangement) | El botón que devuelve al Arrangement, en tres contextos | 3.6 |
| `StatusBarL12.png` | "The Status Bar." | La barra de estado inferior | 3.2 |
| `LiveBrowserL12.png` | "The Browser." | El browser que abre el interruptor de la sección 1 | 3.3 |

URL ejemplo completa: `https://ableton-production.imgix.net/live-manual/12/ControlBarL12.png`. El marcado añade parámetros de ancho (`?auto=&w=320`) y variantes responsivas.

---

## 4. Comparativa: control bar de Live frente al topbar de AI-Track

Estado actual de AI-Track (`crates/app/src/views/topbar.rs`, alto `TOP_BAR_H = 48`): el topbar **es** el titlebar. Izquierda: inset de ventana (80 px en macOS), logo + nombre "AI-Track" + punto de "sin guardar", botón Archivo, tres interruptores de paneles (Browser, Detalle, Mezclador), selector segmentado Session/Arrange. Centro: Play, Stop, Record, Loop, Metrónomo, campo BPM (96 px), compás (56), posición (88) y escala (136, con badge Camelot). Derecha: medidores, preferencias, asistente IA; en Windows, botones de ventana propios.

| Aspecto | Live (control bar) | AI-Track (topbar) | Veredicto |
|---|---|---|---|
| Identidad (logo + nombre) | No hay | Logo y nombre a la izquierda, 20 px + texto LG | **Sobra**: ocupa el sitio más valioso para algo que no se usa |
| Selector Session/Arrange | Extremo derecho, atajo Alt/Option 1 y 2 | Izquierda junto a la identidad | **Mover**: separarlo de la identidad; ponerlo en un extremo y dar atajo |
| Toggles de paneles | Solo browser (izquierda) más menú de configuración | Browser, Detalle y Mezclador como tres iconos | Aceptable; agrupar en un único grupo con separadores |
| Menú de archivo | Barra de menús del sistema | Botón-carpeta con menú | **Mejorar**: en macOS usar la barra de menús nativa; en Windows, menú tradicional |
| Tempo y compás | Grupo propio, número grande | Campos con borde y fondo de 32 px | Bien; falta **Tap tempo**, **Link** y **seguir tempo** |
| Posición | Campo editable | Campo no editable | **Falta**: editarla (clic para escribir) |
| Follow | Interruptor | No existe | **Falta** |
| Escala | Contextual a la selección | Escala de proyecto, ancho 136 | Diferente por diseño (nuestra escala es global); bien, pero estrecharla |
| Transporte | Play/Stop/Record agrupados | Play/Stop/Record, Loop, Metrónomo en una fila | Bien; **separar** loop/metrónomo del transporte con un divisor |
| Overdub, armar automatización, Re-enable, Capture MIDI | Grupo 6 | No existe | **Falta** (ver prioridad 3) |
| Loop y punch | Grupo 7 con campos | Solo botón Loop | **Falta** punch in/out y campos de loop |
| CPU, sample rate | Grupo 8 | En la barra de estado (comentario del código) | Correcto, pero Live los deja visibles; valorar un CPU mini-medidor |
| Medidores, preferencias | Preferencias en menú | Iconos a la derecha | Preferencias puede salir a la barra de menús |
| Asistente IA | No existe | Icono a la derecha | **Nuestro diferenciador**: destacarlo |
| Barra de estado | Mensajes y detalle de nota | Existe (`STATUS_BAR_H`) | Reutilizar para el texto contextual |
| Agrupación | 9 grupos con huecos fijos | 3 bloques (izq./centro/der.) pero sin separadores internos | **Mejorar**: grupos con separadores sutiles |
| Fusión con la barra de título | No (barra nativa aparte) | Sí (altura 48 con inset de semáforos) | Decisión de diseño; ver recomendación 1 |

### Por qué el titlebar actual no convence (hipótesis a partir del código)

1. **Cuatro cosas distintas compiten en el lado izquierdo** (marca, archivo, paneles, vista) y todas tienen el mismo peso, mientras el transporte flota al centro sin relación con ellas.
2. **El inset de 80 px de macOS** deja un hueco muerto antes del logo, y el logo y el nombre repiten información que ya muestra el sistema.
3. **No hay jerarquía de tamaños**: los cuatro campos del transporte tienen la misma altura (32) y borde, sin un "número protagonista".
4. **Sin separadores ni nombres de grupo**: es una fila continua de iconos.
5. **El selector Session/Arrange** está en la esquina donde se espera una marca, no donde Live lo pone (extremo derecho).
6. **El título del proyecto no aparece**: ni nombre de archivo ni indicador claro de guardado más allá de un punto de 6 px.

---

## 5. Recomendaciones para AI-Track

Ordenadas por prioridad. Todas afectan a `crates/app/src/views/topbar.rs`, tema, iconos y movimiento según las reglas del repo (colores del tema, espaciado múltiplo de 4, iconos de `icons.rs`).

### Prioridad 1: ordenar el titlebar (cambios de maquetación, bajo riesgo)

1. **Quitar la marca del titlebar.** Eliminar el texto "AI-Track" y dejar, como mucho, el logo de 16 px dentro del inset de macOS. Mostrar en su lugar el **nombre del proyecto** (con el punto de sin guardar), en `TEXT_MD`, color `muted`, truncado. Es lo que el usuario necesita saber de un vistazo.
2. **Nueva distribución en cuatro zonas con separadores de 1 px** (`border()`), siguiendo el principio de grupos nombrados:

```
┌ inset ┬ ARCHIVO·PANELES ┬ TEMPO·COMPÁS·ESCALA ┬ POSICIÓN·TRANSPORTE ┬ LOOP·REC AVANZADO ┬ ESTADO·VISTA·IA ┐
│ 80px  │ [≡][▤][▥][▦]    │ 120.00  4/4  Cm     │ 12.3.1  ▶ ■ ●       │ ⟲ ⏺ ⌁            │ CPU  [Session|Arr] ✦ │
└───────┴─────────────────┴─────────────────────┴──────────────────────┴───────────────────┴─────────────────────┘
```

3. **Mover el selector Session/Arrange al extremo derecho**, junto al asistente, con los atajos `Alt/Option+1` y `Alt/Option+2` (iguales a Live) en el tooltip.
4. **Jerarquía de tamaños**: BPM y posición en `TEXT_XL` y fuente monoespaciada; compás y escala en `TEXT_MD`; Play/Stop/Record de 32 px y el resto de botones de 28 px. Un solo "número protagonista".
5. **Estilo de campo más plano**: quitar el borde de los campos de lectura y dejar el fondo `panel` con borde solo al pasar el cursor o editar.

### Prioridad 2: funciones que faltan en el centro de la barra

6. **Posición editable** (clic y escribir `compás.negra.semicorchea`) con comando de modelo/engine para saltar.
7. **Interruptor Follow** (la vista sigue el cursor), con estado en `ws` y no en el proyecto.
8. **Tap tempo** y menú del BPM (doble de tempo, mitad, redondear).
9. **Selector de cuantización de lanzamiento global** (Session), como el *Quantization Chooser* de Live.
10. **Mini medidor de CPU** al lado de la vista (valor + barra de 4 px); el resto de lecturas siguen en la barra de estado.

### Prioridad 3: grabación y loop (requieren motor)

11. **Loop con campos** (inicio y longitud) y **punch in/out**.
12. **Overdub MIDI, armar automatización y Re-enable automation** (este último solo visible cuando hay parámetros anulados), y **Capture MIDI**.
13. **Grabar en Session** distinto de Grabar en Arrangement, como en Live.

### Prioridad 4: integración con el sistema

14. **macOS: barra de menús nativa** (Archivo, Edición, Vista, Ayuda) y retirar el botón de carpeta del titlebar. **Windows: barra de menú tradicional** o menú hamburguesa único.
15. **Preferencias** al menú, no al titlebar; liberar sitio para la IA.
16. **Asistente IA** como único elemento con acento de color en la zona derecha (es nuestro diferenciador frente a Live).
17. **Ayuda in-app** siguiendo las lecciones de §1.e: página "Conceptos" con numeración citable, tooltips con la plantilla `Parte - función`, y buscador.

### Prioridad 5: pulido

18. **Estado vacío de la escala** ("No key") más discreto; el badge Camelot solo al pasar el cursor.
19. **Tiempos de animación** del `motion.rs` para el resaltado de Re-enable y el cambio de vista; sin animaciones nuevas fuera de ese módulo.
20. **Modo compacto**: por debajo de cierto ancho, colapsar el grupo de loop/rec avanzado en un menú "⋯" en vez de recortar con `overflow_hidden`.

### Orden sugerido de implementación

| Orden | Entregable | Archivos probables | Estado |
|---|---|---|---|
| 1 | Reestructurar zonas, quitar marca, mover selector de vista | `views/topbar.rs`, `theme.rs` | ⬜ |
| 2 | Jerarquía de tamaños y separadores | `views/topbar.rs`, `widgets.rs` | ⬜ |
| 3 | Posición editable, Follow, Tap tempo | `views/topbar.rs`, `state.rs`, `engine` | ⬜ |
| 4 | Menús nativos y retirar iconos de sistema | `workspace.rs`, `views/topbar.rs` | ⬜ |
| 5 | Loop/punch, overdub, Capture MIDI | `engine`, `model` (comandos) | ⬜ |
| 6 | Ayuda in-app con conceptos y búsqueda | `crates/app`, `docs/` | ⬜ |

---

## 6. Fuentes

- Manual de Live 12, portada y TOC: https://www.ableton.com/en/manual/welcome-to-live/ (redirige a https://www.ableton.com/en/live-manual/12/welcome-to-live/)
- Live Concepts, sección 3.1 *The Control Bar*: https://www.ableton.com/en/live-manual/12/live-concepts/#the-control-bar
- Live Concepts, sección 3.2 *The Status Bar*: https://www.ableton.com/en/live-manual/12/live-concepts/#the-status-bar
- Selector de versión (Live 11): https://www.ableton.com/en/live-manual/11/welcome-to-live/
- Capturas (CDN): https://ableton-production.imgix.net/live-manual/12/ControlBarL12.png y demás archivos citados en §3.e
- Informe interno de contexto: [`03-daw-workflow-ux.md`](03-daw-workflow-ux.md) §1 (modelo de Live) y §2.d (asistente de FL)
- Código analizado: `crates/app/src/views/topbar.rs`
