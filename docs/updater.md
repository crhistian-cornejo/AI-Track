# Actualizaciones automáticas: contrato entre la app y los releases

La app comprueba si hay una versión nueva, la descarga en segundo plano, verifica su firma y la instala al cerrar (o al pulsar "Reiniciar para actualizar"). Si algo falla, "Buscar actualizaciones…" repite el proceso a mano y reinicia la app al terminar. Este documento fija lo que la app espera de cada release y lo que el pipeline de release tiene que producir.

## Manifiesto

- URL fija: `https://github.com/crhistian-cornejo/AI-Track/releases/latest/download/latest.json`. GitHub sirve ahí el asset del último release **publicado** (no borrador ni prerelease).
- `AITRACK_UPDATE_URL` la sustituye (pruebas, canales futuros); `AITRACK_UPDATE=0` desactiva el actualizador.
- Formato:

```json
{
  "version": "0.0.2",
  "notes": "Texto en Markdown con los cambios.",
  "pub_date": "2026-11-20T12:00:00Z",
  "platforms": {
    "macos-universal": {
      "url": "https://github.com/crhistian-cornejo/AI-Track/releases/download/v0.0.2/AI-Track-0.0.2-macos-universal.app.tar.gz",
      "signature": "<contenido completo del archivo .minisig>",
      "sha256": "<hex>",
      "size": 123456789
    },
    "windows-x86_64": {
      "url": "https://github.com/crhistian-cornejo/AI-Track/releases/download/v0.0.2/AI-Track-0.0.2-windows-x86_64-setup.exe",
      "signature": "<contenido completo del archivo .minisig>",
      "sha256": "<hex>",
      "size": 123456789
    }
  }
}
```

- `version` es semver sin `v`. La app la compara con `CARGO_PKG_VERSION` (la versión del workspace) y solo actualiza a una **mayor**.

## Firma

- [minisign](https://jedisct1.github.io/minisign/) (Ed25519). La clave pública está en `release/updater.pub` y la app la incrusta en el binario (`include_str!`). La privada nunca entra al repo: vive en el secreto `UPDATER_PRIVATE_KEY` de GitHub (y su contraseña en `UPDATER_KEY_PASSWORD`), y su copia de seguridad la guarda el dueño del proyecto. **Si se pierde, las versiones instaladas ya no pueden actualizarse solas.**
- La app rechaza cualquier descarga cuya firma no verifique con esa clave o cuyo `sha256`/`size` no coincidan, y lo informa como fallo (no instala nada).
- La firma minisign es independiente de la de Apple (Developer ID) y la de Windows (Authenticode); las tres conviven.

## Artefactos por plataforma

- **macOS** (`macos-universal`): `AI-Track-<v>-macos-universal.app.tar.gz`, un tar.gz cuya única entrada raíz es `AI-Track.app/`. Además, para la descarga manual, `AI-Track-<v>-macos-universal.dmg`.
- **Windows** (`windows-x86_64`): `AI-Track-<v>-windows-x86_64-setup.exe`, instalador NSIS por usuario que acepta:
  - `/S`: instalación silenciosa (sin ventanas) sobre la instalación existente.
  - `/RELAUNCH`: al terminar, vuelve a abrir `aitrack.exe`.
  - Además, `AI-Track-<v>-windows-x86_64-portable.zip` para la descarga manual (los portables no se autoactualizan).

## Cómo instala la app

1. **Comprobar**: 30 s después de arrancar y cada 6 h mientras está abierta; y al pulsar "Buscar actualizaciones…". Sin red, falla en silencio en la comprobación automática; en la manual, lo dice.
2. **Descargar**: en segundo plano a `config_dir()/updates/<version>/`, con reanudación si se corta, verificando `size`, `sha256` y la firma. Durante la grabación o la exportación no empieza a descargar.
3. **Listo**: aviso discreto en la barra de estado "Actualización 0.0.2 lista · Reiniciar". Con la opción "Instalar automáticamente" (activada por defecto), se instala al cerrar la app.
4. **Instalar** (siempre con el proyecto guardado o el autosave al día, y nunca mientras se graba):
   - **macOS**: extrae el `.app` en una carpeta temporal **del mismo volumen** que el bundle actual, renombra el actual a `AI-Track.app.old`, mueve el nuevo a su sitio, relanza con `open -n` y borra el viejo al arrancar. Si el bundle no se puede escribir (permisos), avisa y ofrece abrir el DMG.
   - **Windows**: lanza `setup.exe /S /RELAUNCH` y cierra la app; el instalador espera a que el proceso termine, sustituye los archivos y la vuelve a abrir.
5. **Fallo** en cualquier paso: aviso "No se pudo actualizar · Buscar actualizaciones…", el error completo en el log y los archivos temporales borrados. La versión instalada nunca queda a medias.

## Preferencias

En Preferencias › General (o la página que corresponda):
- "Buscar actualizaciones automáticamente" (activado).
- "Descargar e instalar automáticamente" (activado).
- La versión instalada, la fecha de la última comprobación, el botón "Buscar actualizaciones…" y las notas de la versión disponible.

En el menú de la app: "Buscar actualizaciones…".
