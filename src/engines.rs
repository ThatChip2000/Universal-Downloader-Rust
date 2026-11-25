use crate::config::{get_appdata_bin_dir, save_config, AppConfig};
use crate::models::{
    normalize_version, APP_VERSION, YTDLP_EXE_DOWNLOAD_URL, YTDLP_PYPI_API, YTDLP_RELEASE_API,
};
use flate2::read::ZlibDecoder;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    active_pid: Arc<Mutex<Option<u32>>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
        if let Ok(mut guard) = self.active_pid.lock() {
            *guard = None;
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn kill_pid_sync(pid: u32) {
        #[cfg(target_os = "windows")]
        {
            let mut cmd = Command::new("taskkill");
            cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
            apply_no_window(&mut cmd);
            let _ = cmd.output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }

    pub fn set_pid(&self, pid: Option<u32>) {
        let mut late_kill: Option<u32> = None;
        if let Ok(mut guard) = self.active_pid.lock() {
            if let Some(p) = pid {
                if self.is_cancelled() {
                    *guard = None;
                    late_kill = Some(p);
                } else {
                    *guard = Some(p);
                }
            } else {
                *guard = None;
            }
        }
        if let Some(p) = late_kill {
            std::thread::spawn(move || Self::kill_pid_sync(p));
        }
    }

    /// Marca la cancelación y termina el subproceso activo en segundo plano sin bloquear el hilo UI.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let pid_opt = self.active_pid.lock().ok().and_then(|mut g| g.take());
        if let Some(pid) = pid_opt {
            std::thread::spawn(move || Self::kill_pid_sync(pid));
        }
    }

    /// Marca la cancelación y termina el subproceso activo de forma síncrona (para cierre de ventana).
    pub fn cancel_blocking(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let pid_opt = self.active_pid.lock().ok().and_then(|mut g| g.take());
        if let Some(pid) = pid_opt {
            Self::kill_pid_sync(pid);
        }
    }
}

pub fn safe_replace_file(src: &Path, dst: &Path) -> io::Result<()> {
    if dst.exists() {
        let _ = fs::remove_file(dst);
    }
    fs::rename(src, dst)
}

static EMBEDDED_YTDLP_ZLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/yt-dlp.exe.zlib"));
static EMBEDDED_FFMPEG_ZLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ffmpeg.exe.zlib"));
static EMBEDDED_QJS_ZLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/qjs.exe.zlib"));

pub struct EnginePaths {
    pub ytdlp: PathBuf,
    pub ffmpeg: Option<PathBuf>,
    pub qjs: Option<PathBuf>,
}

fn prepend_bin_dir_to_path(bin_dir: &Path) {
    let bin_str = bin_dir.to_string_lossy().to_string();
    let current = env::var("PATH").unwrap_or_default();
    let already = current
        .split(';')
        .any(|p| p.eq_ignore_ascii_case(bin_str.as_str()));
    if !already {
        let updated = if current.is_empty() {
            bin_str
        } else {
            format!("{bin_str};{current}")
        };
        env::set_var("PATH", updated);
    }
}

fn extract_zlib_if_missing(compressed: &[u8], target: &Path) -> io::Result<()> {
    if target.is_file() && fs::metadata(target).map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(());
    }
    let tmp = target.with_extension("tmp");
    let res: io::Result<()> = (|| {
        let mut decoder = ZlibDecoder::new(compressed);
        let mut out = File::create(&tmp)?;
        io::copy(&mut decoder, &mut out)?;
        out.flush()?;
        drop(out);
        safe_replace_file(&tmp, target)?;
        Ok(())
    })();
    if res.is_err() && tmp.is_file() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Garantiza que yt-dlp.exe, ffmpeg.exe y qjs.exe existan en %LOCALAPPDATA%\UniversalDownloader\bin.
/// Si ya existen, no los sobreescribe (preservando actualizaciones previas de yt-dlp.exe).
pub fn ensure_bundled_engines<F>(mut log_cb: F) -> EnginePaths
where
    F: FnMut(String),
{
    let bin_dir = get_appdata_bin_dir();
    prepend_bin_dir_to_path(&bin_dir);

    let ytdlp_path = bin_dir.join("yt-dlp.exe");
    let ffmpeg_path = bin_dir.join("ffmpeg.exe");
    let qjs_path = bin_dir.join("qjs.exe");

    for (name, bytes, target) in [
        ("yt-dlp.exe", EMBEDDED_YTDLP_ZLIB, &ytdlp_path),
        ("ffmpeg.exe", EMBEDDED_FFMPEG_ZLIB, &ffmpeg_path),
        ("qjs.exe", EMBEDDED_QJS_ZLIB, &qjs_path),
    ] {
        let existed = target.is_file();
        match extract_zlib_if_missing(bytes, target) {
            Ok(()) => {
                if !existed {
                    log_cb(format!("[INFO] Motor integrado extraído: {name}"));
                }
            }
            Err(err) => {
                log_cb(format!("[AVISO] No se pudo extraer {name}: {err}"));
            }
        }
    }

    EnginePaths {
        ytdlp: ytdlp_path,
        ffmpeg: ffmpeg_path.is_file().then_some(ffmpeg_path),
        qjs: qjs_path.is_file().then_some(qjs_path),
    }
}

/// Devuelve los argumentos CLI de runtimes JavaScript (EJS) para yt-dlp.exe.
///
/// ¿Por qué se incluye QuickJS (qjs.exe)?
///   YouTube protege las descargas de su reproductor web mediante desafíos JavaScript (EJS).
///   Empaquetar qjs.exe (~1.8 MB) garantiza portabilidad 100% en cualquier PC con Windows limpio
///   sin exigir Node.js ni Deno instalados.
///
/// ¿Cuándo se activa?
///   yt-dlp aplica prioridades automáticas: Deno (1000) > Node.js (900) > Bun (880) > QuickJS (850).
///   Si el PC tiene Deno, Node.js o Bun instalados, yt-dlp usa ese motor JIT de mayor prioridad;
///   si no encuentra ninguno, activa automáticamente el qjs.exe empaquetado como respaldo.
pub fn ytdlp_js_cli_args(qjs_path: Option<&Path>) -> Vec<String> {
    let qjs_arg = match qjs_path {
        Some(p) if p.is_file() => format!("quickjs:{}", p.display()),
        _ => "quickjs".to_string(),
    };
    vec![
        "--js-runtimes".to_string(),
        "deno".to_string(),
        "--js-runtimes".to_string(),
        "node".to_string(),
        "--js-runtimes".to_string(),
        "bun".to_string(),
        "--js-runtimes".to_string(),
        qjs_arg,
        "--remote-components".to_string(),
        "ejs:github".to_string(),
    ]
}

pub fn apply_no_window(cmd: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

fn file_mtime_rounded(path: &Path) -> Option<f64> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    Some((secs * 100.0).round() / 100.0)
}

pub fn detect_local_ytdlp_version(ytdlp_path: &Path, cfg: &mut AppConfig) -> String {
    if !ytdlp_path.is_file() {
        return "desconocida".to_string();
    }
    let current_mtime = file_mtime_rounded(ytdlp_path);
    if let (Some(cm), Some(saved_m), Some(saved_v)) = (
        current_mtime,
        cfg.ext_engine_mtime,
        cfg.ext_engine_version.as_ref(),
    ) {
        if (cm - saved_m).abs() < 0.02 && !saved_v.trim().is_empty() {
            return saved_v.clone();
        }
    }

    let mut cmd = Command::new(ytdlp_path);
    cmd.arg("--version");
    apply_no_window(&mut cmd);
    if let Ok(output) = cmd.output() {
        if output.status.success() {
            let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !ver.is_empty() {
                cfg.ext_engine_mtime = current_mtime;
                cfg.ext_engine_version = Some(ver.clone());
                save_config(cfg);
                return ver;
            }
        }
    }

    cfg.ext_engine_version
        .clone()
        .unwrap_or_else(|| "desconocida".to_string())
}

pub fn fetch_latest_remote_version() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(format!("UniversalDownloader/{APP_VERSION}"))
        .build()
        .ok()?;

    if let Ok(resp) = client.get(YTDLP_RELEASE_API).send() {
        if let Ok(val) = resp.json::<serde_json::Value>() {
            if let Some(tag) = val.get("tag_name").and_then(|v| v.as_str()) {
                let clean = tag.trim();
                if !clean.is_empty() {
                    return Some(clean.to_string());
                }
            }
        }
    }

    if let Ok(resp) = client.get(YTDLP_PYPI_API).send() {
        if let Ok(val) = resp.json::<serde_json::Value>() {
            if let Some(ver) = val
                .get("info")
                .and_then(|i| i.get("version"))
                .and_then(|v| v.as_str())
            {
                let clean = ver.trim();
                if !clean.is_empty() {
                    return Some(clean.to_string());
                }
            }
        }
    }

    None
}

pub fn is_newer_version(remote: &str, local: &str) -> bool {
    normalize_version(remote) > normalize_version(local)
}

pub fn download_ytdlp_update<F, C>(
    ytdlp_path: &Path,
    mut progress_cb: F,
    is_cancelled: C,
) -> Result<(), String>
where
    F: FnMut(f32),
    C: Fn() -> bool,
{
    let temp_target = ytdlp_path.with_extension("tmp");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(format!("UniversalDownloader/{APP_VERSION}"))
        .build()
        .map_err(|e| format!("Error creando cliente HTTP: {e}"))?;

    let res: Result<(), String> = (|| {
        if is_cancelled() {
            return Err("Actualización cancelada por el usuario.".to_string());
        }
        let mut resp = client
            .get(YTDLP_EXE_DOWNLOAD_URL)
            .send()
            .map_err(|e| format!("No se pudo descargar yt-dlp.exe: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Respuesta HTTP inválida: {}", resp.status()));
        }

        let total = resp.content_length().unwrap_or(0);
        let mut downloaded: u64 = 0;
        let mut buf = vec![0u8; 256 * 1024];
        let mut out =
            File::create(&temp_target).map_err(|e| format!("Error creando archivo temporal: {e}"))?;

        loop {
            if is_cancelled() {
                return Err("Actualización cancelada por el usuario.".to_string());
            }
            let n = resp
                .read(&mut buf)
                .map_err(|e| format!("Error leyendo flujo de actualización: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("Error escribiendo actualización en disco: {e}"))?;
            downloaded += n as u64;
            if total > 0 {
                progress_cb((downloaded as f32) / (total as f32));
            }
        }
        out.flush()
            .map_err(|e| format!("Error cerrando archivo temporal: {e}"))?;
        drop(out);

        let sz = fs::metadata(&temp_target).map(|m| m.len()).unwrap_or(0);
        if sz <= 1_000_000 {
            return Err("El archivo descargado es demasiado pequeño o está corrupto.".to_string());
        }

        safe_replace_file(&temp_target, ytdlp_path)
            .map_err(|e| format!("Error reemplazando yt-dlp.exe: {e}"))?;
        Ok(())
    })();

    if res.is_err() && temp_target.is_file() {
        let _ = fs::remove_file(&temp_target);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cancel_token_state_and_reset() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        token.set_pid(None);
        token.reset();
        assert!(!token.is_cancelled());
    }
}
