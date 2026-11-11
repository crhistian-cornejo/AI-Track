# AI-Track

DAW de música electrónica en Rust. Interfaz GPUI (el mismo `gpui-pre` parcheado que monoBOT), solo macOS y Windows, licencia MIT. El plan maestro está en `PLAN.md` y la investigación en `docs/research/`.

## Mapa

| Ruta | Qué es |
|---|---|
| `crates/model` | Modelo puro del proyecto, mapa de tempo, escalas, **Command API** y undo/propuestas. Sin E/S ni GPUI. |
| `crates/dsp` | Trait `Processor`, `AudioBuffer`, eventos, parámetros y primitivas DSP. Sin asignaciones en `process`. |
| `crates/stock` | Plugins propios (EQ, compresor, limitador, reverb, synth, sampler…) y sus presets de fábrica. |
| `crates/engine` | Motor de audio: grafo compilado fuera del hilo RT, transporte, E/S con cpal, MIDI. |
| `crates/host` | Alojamiento de plugins VST3 y CLAP (no hay VST2), escáner y caché. |
| `crates/library` | Librería inteligente de samples (SQLite, carpetas virtuales por tonalidad/tipo) y banco de sonidos. |
| `crates/ai` | Audio→MIDI, separación de stems, tonalidad/BPM, gestor de modelos ONNX descargados bajo demanda. |
| `crates/app` | Binario `aitrack`: la interfaz GPUI. Solo pinta y conecta; la lógica vive en los otros crates. |
| `xtask` | Tareas: `cargo xtask soundbank` (descarga el banco de sonidos), empaquetado. |
| `vendor/gpui-pre*` | GPUI fijado y parcheado. No lo actualices. |
| `reference/` | Repos de referencia, ignorados por git. Solo lectura; nunca se compilan. |

## Reglas

- **Licencias:** el proyecto es MIT. No se copia código de vibez, Lemon-Studio ni Pro-Wave (GPL-3): solo ideas. De futureboard-studio (MIT) se puede adaptar código citándolo en `NOTICE` y con un comentario `// Adapted from futureboard-studio: <ruta>`. Ninguna dependencia GPL/LGPL/AGPL (`deny.toml`).
- **Hilo de audio:** nada de asignaciones, locks, strings, E/S ni `println!` dentro de `Processor::process` ni del callback. Lo retirado se libera fuera del hilo RT.
- **Toda edición del proyecto es un `Command`** de `aitrack-model`. La UI, los scripts y los agentes de IA editan solo con comandos, nunca mutando el `Project` directamente.
- **Interfaz:** los colores salen del tema (`crates/app/src/theme.rs`), los tiempos y curvas de `crates/app/src/motion.rs`, los iconos de `crates/app/src/icons.rs`. Nada de `rgb(0x…)` sueltos en las vistas. Espaciado en múltiplos de 4 px.
- Los tests van junto al código (`#[cfg(test)] mod tests`).
- Comprobar antes de terminar: `cargo check --workspace`, `cargo test -p <crate>`, `cargo clippy -p <crate>`.
