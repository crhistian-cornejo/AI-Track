# Lanzamiento 0.0.1: versión instalable para macOS y Windows

Objetivo: que cualquiera descargue AI-Track desde GitHub Releases, lo instale con doble clic en macOS (Apple Silicon e Intel) y Windows 10/11 (x64), y pueda hacer una canción sin abrir una terminal. La 0.0.1 es una **preview pública**: sin actualizaciones automáticas ni tiendas, pero firmada si hay cuentas, y con un camino claro para reportar fallos.

## 1. Qué contiene un instalable

| Pieza | Origen | En el paquete |
|---|---|---|
| `aitrack` (app GPUI) | `crates/app` | Sí |
| `aitrack-plugin-host` (sandbox de plugins) | `crates/host/src/bin` | Sí, junto al ejecutable (`sandbox::HOST_EXE`) |
| `aitrack-plugin-scanner` | `crates/host/src/bin` | Sí, junto al ejecutable (`scan::SCANNER_EXE`) |
| `aitrack-ml-worker` (IA en proceso aparte) | `crates/ai/src/bin` | Sí, junto al ejecutable |
| Presets de fábrica de los dispositivos (3,6 MB) | `crates/stock/presets` | Ya van dentro del binario (`build.rs` los incrusta) |
| `content/plugins/manifest.toml`, `content/soundbank/manifest.toml` | repo | Sí, en `content/` junto al ejecutable (`real/library.rs` lo busca ahí) |
| `content/soundbank/factory.db` (1,8 MB) | `cargo xtask soundbank index` | Sí |
| Banco de sonidos CC0 (766 MB, 12 packs) | `cargo xtask soundbank fetch` | **No en el instalador**: se descarga en el primer arranque (ver §4) |
| ONNX Runtime, modelos de IA | GitHub de Microsoft / Hugging Face | No: ya se descargan bajo demanda (`ai::models`) |
| `libmp3lame` | — | No: es LGPL; el usuario lo instala (ya hay mensaje en `lame_missing_message`) |

## 2. Cambios en el código antes de empaquetar

1. **Versión**: `Cargo.toml` del workspace pasa de `0.1.0` a `0.0.1`; mostrarla en Preferencias y en el título del instalador. Etiqueta git `v0.0.1`.
2. **Permiso de micrófono (macOS)**: el `Info.plist` necesita `NSMicrophoneUsageDescription`; sin él, al grabar o abrir una entrada de audio macOS deniega el acceso. Igual para el teclado/MIDI no hace falta nada.
3. **Rutas de recursos**: hoy `real/library.rs` busca `content/` junto al ejecutable o en el árbol de fuentes (`CARGO_MANIFEST_DIR`). En el `.app` el ejecutable está en `Contents/MacOS/` y los recursos deben ir en `Contents/Resources/content`: añadir ese candidato. Quitar la ruta de fuentes en builds `release`.
4. **Primer arranque**: proyecto vacío (ya), pantalla de bienvenida con "Descargar banco de sonidos (766 MB)", "Abrir demo" y el aviso de privacidad del asistente de IA.
5. **Registro y fallos**: escribir un log rotatorio en `config_dir()/logs/` y un `panic` hook que guarde `crash-<fecha>.txt` y, al reabrir, ofrezca "Copiar informe" y abrir un issue en GitHub (sin enviar nada solo).
6. **Medir el tamaño del binario** (pendiente de F0) y activar en `release`: `lto = "thin"`, `codegen-units = 1`, `strip = "debuginfo"` (ojo: los símbolos para informes de fallos se guardan aparte, no se tiran).
7. **Windows**: CRT estático (`-C target-feature=+crt-static`) para no depender del redistribuible de VC++; icono y metadatos del `.exe` con un `.rc` (crate `winresource`, MIT).

## 3. Empaquetado: `cargo xtask package`

Un solo comando, igual en local y en CI, que deja los artefactos en `target/dist/`:

- **macOS**
  - Compilar `aarch64-apple-darwin` y `x86_64-apple-darwin` en release y unirlos con `lipo` (binario universal) para los cuatro ejecutables.
  - Montar `AI-Track.app`: `Info.plist` (id `dev.aitrack.daw`, igual que el `app_id` de la ventana; versión; `LSMinimumSystemVersion` 13.0; `NSMicrophoneUsageDescription`; `NSHighResolutionCapable`), icono `.icns` generado desde `assets/brand/aitrack-mark.svg`, helpers en `Contents/MacOS/`, `content/` en `Contents/Resources/`.
  - Firma con **hardened runtime** y estas *entitlements* (las necesita un DAW):
    - `com.apple.security.device.audio-input` (grabar),
    - `com.apple.security.cs.disable-library-validation` (cargar VST3/CLAP de terceros y la `onnxruntime.dylib` descargada),
    - `com.apple.security.cs.allow-unsigned-executable-memory` y `allow-jit` solo en `aitrack-plugin-host` (plugins con JIT).
  - Notarizar con `notarytool` y "grapar" el ticket (`stapler`).
  - Crear `AI-Track-0.0.1-macos-universal.dmg` (con enlace a Aplicaciones) y firmarlo.
- **Windows**
  - Compilar `x86_64-pc-windows-msvc` en release.
  - Instalador **NSIS** por usuario (sin permisos de administrador) en `%LOCALAPPDATA%\Programs\AI-Track`, acceso directo en el menú Inicio, desinstalador, asociación de la extensión de proyecto. Alternativa: MSI con WiX si algún día se distribuye en empresas.
  - Firmar `.exe` e instalador con **Azure Trusted Signing** (o un certificado OV/EV) y sello de tiempo.
  - Además, un `.zip` portable (con `AITRACK_CONFIG_DIR` relativo) para quien no quiera instalar.
- Generar `SHA256SUMS.txt` de todos los artefactos.

Herramienta: implementarlo en `xtask` a mano (control total, sin dependencias nuevas) o apoyarse en `cargo-packager` (MIT/Apache, hace `.app`, DMG, NSIS y firma). Recomendación: `cargo-packager` para DMG/NSIS y el resto (lipo, entitlements, notarización) en `xtask`.

## 4. Banco de sonidos en el primer arranque

766 MB no caben en un instalador razonable. El Content Hub ya sabe descargar packs con verificación sha256 (`library::install`, `xtask soundbank`): exponer "Descargar banco de fábrica" en la bienvenida y en Sonidos, con progreso, reanudación y la licencia de cada pack. El `factory.db` del instalador ya lista los samples; los que no están descargados se muestran como "no descargado" y se bajan al auditar o arrastrar, como los de la nube.

## 5. CI de lanzamiento: `.github/workflows/release.yml`

- Se dispara con un tag `v*` (y a mano con `workflow_dispatch`).
- Jobs: `macos-14` (universal, firma + notarización), `windows-latest` (NSIS + zip, firma), los dos ejecutando antes los tests del CI normal.
- Crea un **GitHub Release en borrador** con los artefactos, `SHA256SUMS.txt` y las notas generadas desde los commits; se publica a mano tras probarlo.
- Secretos necesarios (los pone el dueño del repo, nunca en el código):
  - macOS: `APPLE_CERT_P12` + `APPLE_CERT_PASSWORD` (Developer ID Application), `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` (o clave API de App Store Connect para `notarytool`).
  - Windows: credenciales de Azure Trusted Signing (`AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, cuenta y perfil) o el `.pfx` del certificado.

## 6. Pruebas antes de publicar (checklist manual)

En una máquina limpia de cada sistema, sin Rust ni el repo:

- [ ] El DMG/instalador abre sin avisos de Gatekeeper/SmartScreen (firmado) o con el aviso esperado (sin firmar).
- [ ] Primer arranque: bienvenida, descarga del banco, abrir demo, reproducir.
- [ ] Audio: elegir dispositivo, 64/128/256 muestras, sin cortes; grabar audio (pide permiso de micrófono) y MIDI.
- [ ] Plugins: escanear VST3 y CLAP de prueba (los OSS de `content/plugins`), cargar uno en sandbox y uno en proceso, abrir su editor.
- [ ] IA: descarga de ONNX Runtime y de un modelo, audio→MIDI, stems.
- [ ] Guardar, cerrar, reabrir el proyecto; autosave tras un cierre forzado.
- [ ] Exportar WAV y stems; MP3 con LAME instalado y el mensaje sin él.
- [ ] Desinstalar no deja nada fuera de `config_dir()` (que se conserva a propósito).

## 7. Decisiones que tiene que tomar el dueño del proyecto

1. **Firma en macOS**: cuenta Apple Developer (99 USD/año). Sin ella el `.app` funciona pero hay que abrirlo con clic derecho → Abrir, y macOS lo marca como "no verificado".
2. **Firma en Windows**: Azure Trusted Signing (~10 USD/mes) o certificado OV (~200–400 USD/año). Sin firma, SmartScreen muestra "Windows protegió su PC" hasta que el archivo gana reputación.
3. **Telemetría de fallos**: solo informes manuales (propuesto para 0.0.1) o un servicio (Sentry, etc.) con consentimiento explícito.
4. **Nombre definitivo**: "AI-Track" sigue siendo provisional (§10); cambiarlo después del lanzamiento cambia el id del bundle y la carpeta de configuración.

## 8. Orden de trabajo

1. Cambios de código de §2 (versión, Info.plist, rutas de recursos, log y fallos, perfil release).
2. `cargo xtask package` para macOS y Windows sin firmar; probarlo a mano (§6).
3. `release.yml` generando borradores sin firmar.
4. Firma y notarización en cuanto existan las cuentas (§7).
5. Bienvenida y descarga del banco de sonidos (§4).
6. Tag `v0.0.1`, borrador, prueba en máquinas limpias, publicar.

Fuera de la 0.0.1: actualizaciones automáticas (Sparkle en macOS, el propio instalador NSIS en Windows), Mac App Store / Microsoft Store (la tienda de Mac exige sandbox, incompatible con cargar plugins de terceros en proceso), Linux.

---

## 9. Implementación: comandos, secretos y primera versión

Todo está en `xtask` (`package.rs`, `updates.rs`, `icons.rs`), `release/` y `.github/workflows/release.yml`.

### Comandos

```
cargo xtask icons                                   # regenera release/icons/AppIcon.icns y aitrack.ico desde assets/brand/aitrack-logo.svg
cargo xtask package --target mac [--universal] [--sign]   # en un Mac
cargo xtask package --target win [--sign]                 # en Windows (necesita makensis)
cargo xtask package manifest --version 0.0.1 --base-url https://github.com/crhistian-cornejo/AI-Track/releases/download/v0.0.1 --notes-file notes.md
cargo xtask updater-keygen --out <carpeta fuera del repo>   # solo si hubiera que rotar la clave
```

`package` deja en `target/dist/`: `AI-Track-<v>-macos-universal.{app.tar.gz,dmg}`, `AI-Track-<v>-windows-x86_64-{setup.exe,portable.zip}`, un `.minisig` junto a `.app.tar.gz` y `setup.exe`, `*-symbols.*` (dSYM/PDB para symbolicar informes de fallos), `platform-<clave>.json` (fragmento para el manifiesto) y `SHA256SUMS.txt`. `--skip-build` reutiliza los binarios ya compilados.

Decisiones: el perfil `release` usa `lto = "thin"`, `codegen-units = 1`, `debug = "line-tables-only"` y `split-debuginfo = "packed"`: los símbolos van a `.dSYM`/`.pdb` aparte (que se publican como artefacto `symbols`) y no dentro del ejecutable. No se usa `strip = "debuginfo"` porque en MSVC también suprime el `.pdb`. En macOS el paquete ejecuta además `strip -x`. Windows enlaza el CRT estático (`.cargo/config.toml`); `onnxruntime.dll` y los plugins se cargan en ejecución con su propio CRT. El icono y la versión de `aitrack.exe` se incrustan desde `crates/app/build.rs` con `embed-resource` (MIT; ya estaba en el árbol por GPUI; `winresource` no se pudo añadir sin red).

Entitlements (`release/macos/`): `app.entitlements` (audio-input, disable-library-validation) para la app y `aitrack-ml-worker`; `plugin-host.entitlements` (más allow-unsigned-executable-memory y allow-jit) para `aitrack-plugin-host` y `aitrack-plugin-scanner` (el escáner también carga plugins).

### Actualizaciones: la clave ya existe

La clave pública (minisign, id `7BA4992810D26F12`) está en `release/updater.pub` y la app la incrusta. La privada es el secreto `UPDATER_PRIVATE_KEY` (contenido completo del archivo creado con `minisign -G -W`, **sin contraseña**; `UPDATER_KEY_PASSWORD` no hace falta y, si falta o está vacío, se usa la clave sin cifrar). `updater-keygen` queda como ayuda documentada (genera una pareja protegida con contraseña y se niega a sobrescribir `release/updater.pub` sin `--force`); rotar la clave deja sin actualización automática a las versiones ya instaladas.

### Secretos de GitHub (todos opcionales)

| Secreto | Para qué |
|---|---|
| `UPDATER_PRIVATE_KEY` (+ `UPDATER_KEY_PASSWORD` si está cifrada) | Firmar `.app.tar.gz` y `setup.exe`; sin él no hay `latest.json` |
| `APPLE_CERT_P12` (base64), `APPLE_CERT_PASSWORD` | Certificado Developer ID Application |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | Notarización (`notarytool`) |
| `WINDOWS_PFX_BASE64`, `WINDOWS_PFX_PASSWORD` | Authenticode con un `.pfx` (`signtool`) |
| `WINDOWS_SIGN_COMMAND` | Alternativa: comando de firma con `{file}` (p. ej. `azuresigntool sign ... {file}`) para Azure Trusted Signing |

### Lanzar

`release.yml` corre con un tag `v*` (publica) o a mano con `workflow_dispatch` (borrador salvo `publish: true`). Compila `macos-14` (universal) y `windows-latest`, y un último job une los fragmentos en `latest.json`, escribe `SHA256SUMS.txt` y crea el release con `gh release create` (notas = commits desde el tag anterior). Un tag con sufijo (`v0.0.2-rc1`) se marca prerelease. **`releases/latest/download/latest.json` solo resuelve para releases publicados y que no sean prerelease ni borrador**; mientras sea borrador, las apps instaladas no verán la versión.

### Checklist de la primera versión (0.0.1, sin firmas de Apple/Windows)

1. Confirmar que `release/updater.pub` coincide con el secreto `UPDATER_PRIVATE_KEY` (`gh secret list`).
2. Ejecutar el workflow a mano con `publish` desmarcado y descargar el borrador; pasar el checklist de §6 en máquinas limpias.
3. `git tag v0.0.1 && git push origin v0.0.1` (publica) o volver a lanzar a mano con `publish: true`.
4. Comprobar `https://github.com/crhistian-cornejo/AI-Track/releases/latest/download/latest.json`.

Sin certificados la primera versión sale así: el `.app` va firmado ad hoc (con hardened runtime) y sin notarizar, así que Gatekeeper pide clic derecho > Abrir; el instalador de Windows no lleva Authenticode y SmartScreen avisa. Las firmas minisign del actualizador son independientes y funcionan igual. El actualizador de macOS extrae el `.app` desde el tar.gz (sin atributo de cuarentena), por lo que las actualizaciones posteriores no repiten el aviso.
