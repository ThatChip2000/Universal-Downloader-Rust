# Universal Downloader (Rust Edition — v4.0.0)

Versión nativa en **Rust** (`egui` / `eframe`) del proyecto **Universal Downloader**, ubicada de forma independiente respecto a la versión original en Python (`../Universal Downloader`).

Está diseñada para distribuirse como un **único archivo ejecutable portátil** (`Universal Downloader (Rust).exe`) de doble clic en Windows x64, sin consola negra ni dependencias de Python, Node.js o Deno.

---

## Características principales

- **Binario único portátil (`71.597.056` bytes / ~68,3 MB):**
  - En tiempo de compilación (`build.rs`), comprime con `zlib` (`flate2`) los motores en `assets/`:
    - `yt-dlp.exe`
    - `ffmpeg.exe`
    - `qjs.exe` (QuickJS para resolución EJS de YouTube sin Node/Deno)
  - En tiempo de ejecución (`src/engines.rs`), si alguno de esos ejecutables no existe en `%LOCALAPPDATA%\UniversalDownloader\bin\`, lo extrae de forma atómica (`.tmp` + renombrado) y antepone esa carpeta al `PATH` del proceso.
- **Sin ventana de consola:** Compilado con `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` y bandera `CREATE_NO_WINDOW` en todos los subprocesos (`yt-dlp.exe`, `ffmpeg.exe`, `taskkill`, `explorer`).
- **Paridad funcional con `v3.0.4` + botón Cancelar:**
  - **Pestaña Principal:** Descarga individual (Video `MP4`, Audio `MP3`, `GIF`, Imagen) y soporte de **Playlist** con selección múltiple y formato/calidad configurable por fila.
  - **Pestaña Clip:** Sliders duales con separación mínima configurable, cajas de tiempo `MM:SS` editables y recorte preciso con `--download-sections` + `--force-keyframes-at-cuts`.
  - **Suno:** Extractor dedicado y recorte de audio local mediante `ffmpeg.exe`.
  - **X / Twitter (FxTwitter fallback):** Respaldo vía `api.fxtwitter.com/status/{id}` (incluyendo `.m3u8` y fotos) + sondeo paralelo `HEAD`/`Range` de tamaños CDN y ajuste automático para streams HLS > 900 MB.
  - **Conversión a GIF:** Conversión de video descargado a GIF con `ffmpeg.exe` según nivel de calidad elegido.
  - **Imágenes:** Descarga directa y respaldo mediante etiqueta HTML `og:image`.
  - **Cancelación de descarga:** Botón **Cancelar** (`CancelToken` con `AtomicBool` + `taskkill /F /T /PID` silencioso).
  - **Actualizador integrado:** Verificación de versión y auto-actualización de `yt-dlp.exe`.
  - **Temas:** Claro, Oscuro y Automático del sistema (lectura de `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`).
  - **Rendimiento y UX:** Debounce de URL (`300 ms`), caché LRU de metadatos y miniaturas (`32` entradas) y consola de registro de hasta `500` líneas.
- **Compatibilidad de configuración:** Comparte `%LOCALAPPDATA%\UniversalDownloader\config.json` con la versión de Python preservando campos adicionales mediante `#[serde(flatten)]`.

---

## Arquitectura del proyecto

```text
Universal Downloader Rust/
├── Cargo.toml                  # Manifiesto y dependencias (eframe/egui, reqwest, image, serde, etc.)
├── build.rs                    # Compresión zlib de assets/*.exe e incrustación de app_icon.ico y metadatos PE
├── .gitignore                  # Excluye /target/, *.exe y *.zip
├── Universal Downloader (Rust).exe  # Ejecutable portátil final (compilado en release)
├── assets/
│   ├── app_icon.ico
│   ├── ffmpeg.exe              # No versionado en Git
│   ├── qjs.exe                 # No versionado en Git
│   └── yt-dlp.exe              # No versionado en Git
└── src/
    ├── main.rs                 # Punto de entrada GUI (900x770, mín. 780x650) e ícono de ventana
    ├── ui.rs                   # Interfaz egui, estado reactivo, pestañas, sliders, playlist y consola
    ├── models.rs               # Enums, structs de dominio, selección de calidades/formatos y pruebas unitarias
    ├── config.rs               # Lectura y guardado seguro de config.json
    ├── engines.rs              # Descompresión atómica de binarios, JS runtime args (QuickJS) y auto-updater
    ├── theme.rs                # Paletas Oscuro/Claro y detección de tema en Windows
    ├── extractors.rs           # yt-dlp JSON, Suno, FxTwitter fallback, sondeo CDN paralelo y miniaturas 88x50
    └── downloader.rs           # Ejecución de descargas, clips, GIF, imágenes, Suno trim y CancelToken
```

---

## Cómo usar (Usuario final)

1. Copia o mueve **`Universal Downloader (Rust).exe`** a cualquier carpeta en cualquier PC con Windows x64.
2. Haz **doble clic**. En el primer arranque extraerá automáticamente `yt-dlp.exe`, `ffmpeg.exe` y `qjs.exe` en `%LOCALAPPDATA%\UniversalDownloader\bin\` si no estaban presentes.
3. No requiere instalar Python, Rust, Node.js ni Deno.

---

## Compilación desde el código fuente

### Requisitos de desarrollo (Windows x64)
- **Rust** (`stable-x86_64-pc-windows-msvc`, probado con `rustc 1.100.0` / `cargo 1.100.0`)
- **Visual Studio Build Tools** con componente de escritorio C++ x64 (`msvcrt.lib` en `lib\x64`) y **Windows 10/11 SDK** (`rc.exe`)
- Los archivos `yt-dlp.exe`, `ffmpeg.exe`, `qjs.exe` y `app_icon.ico` dentro de `assets/`

> **Nota sobre MSVC en equipos con varias instalaciones de Visual Studio:**  
> Si tienes Visual Studio Community sin librerías x64 de escritorio junto a Visual Studio Build Tools, inicializa primero `vcvars64.bat` de Build Tools antes de invocar `cargo`.

### Comandos

Desde **Símbolo del sistema / cmd** (con entorno BuildTools inicializado):

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "Universal Downloader Rust"

:: Ejecutar pruebas unitarias
cargo test

:: Compilar binario optimizado de producción
cargo build --release
copy /Y "target\release\universal_downloader_rust.exe" "Universal Downloader (Rust).exe"
```

---

## Diferencias clave frente a la versión en Python (`v3.0.4`)

| Aspecto | Python (`Universal Downloader`) | Rust (`Universal Downloader Rust`) |
| --- | --- | --- |
| **Interfaz gráfica** | PySide6 (Qt) | `egui` / `eframe` nativo (`glow` OpenGL) |
| **Empaquetado `.exe`** | PyInstaller (desempaquetado en runtime / intérprete Python + Qt) | Binario nativo compilado en Rust + recursos `.zlib` embebidos |
| **Arranque en frío** | Requiere inicializar Python + PySide6 | Arranque directo nativo |
| **Motor multimedia** | `yt-dlp.exe`, `ffmpeg.exe`, `qjs.exe` | Los mismos binarios embebidos y compatibles |
| **Configuración** | `%LOCALAPPDATA%\UniversalDownloader\config.json` | Misma ruta y esquema compatible |
| **Cancelación activa** | No incluida en `v3.0.4` | Botón **Cancelar** con terminación de árbol de procesos (`taskkill`) |
