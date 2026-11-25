pub use crate::engines::CancelToken;
use crate::engines::{apply_no_window, safe_replace_file, ytdlp_js_cli_args};
use crate::extractors::{
    build_http_client, extract_suno_info, extract_twitter_fallback_info,
    select_twitter_fallback_stream_url,
};
use crate::models::{
    clean_ansi, format_bytes, format_timestamp_input, is_suno_url, is_twitter_gif_info,
    is_twitter_status_url, parse_cli_progress, sanitize_filename, AudioQuality, GifQuality,
    MediaFormat, MediaInfo, UiMessage, VideoQuality,
};
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

/// Ejecuta FFmpeg drenando `stderr` en un hilo dedicado para evitar el bloqueo de pipe (4 KB en Windows).
pub(crate) fn run_ffmpeg_cancellable(cmd: &mut Command, cancel: &CancelToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("Descarga cancelada por el usuario.".into());
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    apply_no_window(cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Error ejecutando FFmpeg: {e}"))?;
    cancel.set_pid(Some(child.id()));

    let stderr = child.stderr.take();
    let err_handle = std::thread::spawn(move || -> Vec<String> {
        let mut tail = Vec::new();
        if let Some(se) = stderr {
            let reader = BufReader::new(se);
            for raw in reader.lines().map_while(Result::ok) {
                let line = clean_ansi(&raw).trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if tail.len() >= 5 {
                    tail.remove(0);
                }
                tail.push(line);
            }
        }
        tail
    });

    let status = child
        .wait()
        .map_err(|e| format!("Error esperando FFmpeg: {e}"))?;
    cancel.set_pid(None);
    let err_tail = err_handle.join().unwrap_or_default();

    if cancel.is_cancelled() {
        return Err("Descarga cancelada por el usuario.".into());
    }
    if !status.success() {
        if err_tail.is_empty() {
            return Err("FFmpeg finalizó con un error al procesar el archivo.".into());
        }
        return Err(err_tail.join("\n"));
    }
    Ok(())
}

fn video_format_selector(
    selected_quality: VideoQuality,
    locked_id: Option<&str>,
    has_ffmpeg: bool,
) -> String {
    if let Some(lid) = locked_id {
        if !lid.is_empty() {
            return format!("{lid}/bestvideo+bestaudio/best");
        }
    }
    let max_h = selected_quality.tier_bounds().map(|(h, _)| h);
    if has_ffmpeg {
        if let Some(h) = max_h {
            format!(
                "bestvideo[height<={h}][protocol^=http][ext=mp4]+bestaudio[protocol^=http][ext=m4a]/\
                 bestvideo[height<={h}][ext=mp4]+bestaudio[ext=m4a]/\
                 bestvideo[height<={h}]+bestaudio/\
                 best[height<={h}]/best"
            )
        } else {
            "bestvideo[protocol^=http][ext=mp4]+bestaudio[protocol^=http][ext=m4a]/\
             bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best"
                .to_string()
        }
    } else if let Some(h) = max_h {
        format!("best[height<={h}][ext=mp4]/best[height<={h}]/best")
    } else {
        "best[ext=mp4]/best".to_string()
    }
}

fn snapshot_dir(dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut map = HashMap::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                    map.insert(p, mtime);
                }
            }
        }
    }
    map
}

fn run_ytdlp_cli(
    ytdlp_path: &Path,
    args: &[String],
    download_dir: &Path,
    status_verb: &str,
    tx: &Sender<UiMessage>,
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, String> {
    if cancel.is_cancelled() {
        return Err("Descarga cancelada por el usuario.".into());
    }
    fs::create_dir_all(download_dir).map_err(|e| format!("Error creando carpeta destino: {e}"))?;
    let before_snapshot = snapshot_dir(download_dir);

    let mut cmd = Command::new(ytdlp_path);
    cmd.args(args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    apply_no_window(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("No se pudo iniciar yt-dlp.exe: {e}"))?;
    cancel.set_pid(Some(child.id()));

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let tx_err = tx.clone();
    let err_handle = std::thread::spawn(move || -> Vec<String> {
        let mut err_lines = Vec::new();
        if let Some(se) = stderr {
            let reader = BufReader::new(se);
            for raw in reader.lines().map_while(Result::ok) {
                let line = clean_ansi(&raw).trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let _ = tx_err.send(UiMessage::Log(format!("[ERROR] {line}")));
                err_lines.push(line);
            }
        }
        err_lines
    });

    let mut last_file: Option<PathBuf> = None;
    let mut out_lines = Vec::new();
    let mut last_ui_tick = Instant::now() - std::time::Duration::from_millis(200);

    if let Some(so) = stdout {
        let reader = BufReader::new(so);
        for raw in reader.lines().map_while(Result::ok) {
            let line = clean_ansi(&raw).trim().to_string();
            if line.is_empty() {
                continue;
            }
            out_lines.push(line.clone());
            if let Some((pct, speed, eta)) = parse_cli_progress(&line) {
                let frac = (pct / 100.0).clamp(0.0, 1.0);
                let now = Instant::now();
                if frac >= 0.99 || now.duration_since(last_ui_tick).as_millis() >= 60 {
                    last_ui_tick = now;
                    let _ = tx.send(UiMessage::Status {
                        text: format!("{status_verb}... Velocidad: {speed} | Restante: {eta}"),
                        progress: Some(frac),
                        percent: Some(format!("{}%", pct as u32)),
                    });
                }
            } else {
                let _ = tx.send(UiMessage::Log(format!("[INFO] {line}")));
                if let Some((_, dest)) = line.split_once("Destination:") {
                    last_file = Some(PathBuf::from(dest.trim()));
                } else if let Some((_, merged)) = line.split_once("Merging formats into") {
                    last_file = Some(PathBuf::from(merged.trim().trim_matches('"')));
                } else if line.contains("[download]") && line.contains("has already been downloaded") {
                    if let Some((_, rest)) = line.split_once("[download]") {
                        if let Some((cand, _)) = rest.split_once("has already been downloaded") {
                            let cand_p = PathBuf::from(cand.trim());
                            if cand_p.is_file() {
                                last_file = Some(cand_p);
                            }
                        }
                    }
                }
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| format!("Error esperando a yt-dlp.exe: {e}"))?;
    cancel.set_pid(None);
    let err_lines = err_handle.join().unwrap_or_default();

    if cancel.is_cancelled() {
        return Err("Descarga cancelada por el usuario.".into());
    }

    if !status.success() {
        let combined: Vec<String> = if !err_lines.is_empty() {
            err_lines.into_iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect()
        } else {
            out_lines.into_iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect()
        };
        return Err(combined.join("\n"));
    }

    if !last_file.as_ref().map(|p| p.is_file()).unwrap_or(false) {
        if let Ok(entries) = fs::read_dir(download_dir) {
            let mut changed: Vec<(PathBuf, SystemTime)> = entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if !p.is_file() {
                        return None;
                    }
                    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ext.eq_ignore_ascii_case("part") || ext.eq_ignore_ascii_case("ytdl") {
                        return None;
                    }
                    let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
                    let is_new = match before_snapshot.get(&p) {
                        Some(prev) => mtime > *prev,
                        None => true,
                    };
                    is_new.then_some((p, mtime))
                })
                .collect();
            changed.sort_by_key(|a| std::cmp::Reverse(a.1));
            if let Some((newest, _)) = changed.into_iter().next() {
                last_file = Some(newest);
            }
        }
    }

    Ok(last_file)
}

pub fn convert_video_to_gif(
    ffmpeg_path: Option<&Path>,
    input_video_path: &Path,
    max_height: Option<u32>,
    fps: u32,
    tx: &Sender<UiMessage>,
    cancel: &CancelToken,
) -> Result<PathBuf, String> {
    let ffmpeg =
        ffmpeg_path.ok_or_else(|| "FFmpeg es necesario para generar archivos GIF.".to_string())?;
    if !ffmpeg.is_file() {
        return Err("No se encontró ffmpeg.exe para generar el GIF.".into());
    }
    if !input_video_path.is_file() {
        return Err("No se encontró el video origen para convertir a GIF.".into());
    }

    let stem = input_video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("animacion");
    let clean_stem = stem.strip_suffix(".gif_tmp").unwrap_or(stem);
    let gif_path = input_video_path.with_file_name(format!("{clean_stem}.gif"));

    let _ = tx.send(UiMessage::Log(
        "Convirtiendo animación a formato .gif optimizado...".into(),
    ));
    let _ = tx.send(UiMessage::Status {
        text: "Generando archivo GIF animado...".into(),
        progress: Some(0.97),
        percent: Some("97%".into()),
    });

    let vf_filter = match max_height {
        Some(h) if h > 0 => format!(
            "fps={fps},scale=-2:'min({h},ih)':flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
        ),
        _ => format!(
            "fps={fps},split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
        ),
    };

    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-y",
        "-threads",
        "0",
        "-i",
        &input_video_path.to_string_lossy(),
        "-vf",
        &vf_filter,
        &gif_path.to_string_lossy(),
    ]);

    match run_ffmpeg_cancellable(&mut cmd, cancel) {
        Ok(()) if gif_path.is_file() => {
            if input_video_path != gif_path {
                let _ = fs::remove_file(input_video_path);
            }
            Ok(gif_path)
        }
        Ok(()) => {
            if input_video_path.to_string_lossy().contains(".gif_tmp") {
                let _ = fs::remove_file(input_video_path);
            }
            Err("FFmpeg no generó el archivo GIF esperado.".into())
        }
        Err(err) => {
            if gif_path.is_file() {
                let _ = fs::remove_file(&gif_path);
            }
            if input_video_path.to_string_lossy().contains(".gif_tmp") {
                let _ = fs::remove_file(input_video_path);
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn download_suno_media(
    url: &str,
    cached_info: Option<&MediaInfo>,
    selected_format: MediaFormat,
    audio_quality: AudioQuality,
    download_dir: &Path,
    ffmpeg_path: Option<&Path>,
    tx: &Sender<UiMessage>,
    cancel: &CancelToken,
) -> Result<PathBuf, String> {
    let info = match cached_info {
        Some(i) if i.suno_mp4_url.is_some() => i.clone(),
        _ => extract_suno_info(url)?,
    };

    fs::create_dir_all(download_dir).map_err(|e| format!("Error creando carpeta destino: {e}"))?;
    let safe_title = sanitize_filename(info.title.as_deref().unwrap_or("Cancion"), 110);
    let song_id = info.id.as_deref().unwrap_or("suno");
    let base_name = format!("{safe_title} [{song_id}]");

    let (target_url, final_path, temp_download_path) = match selected_format {
        MediaFormat::ImageGif => {
            let u = info
                .suno_img_url
                .clone()
                .ok_or_else(|| "No se encontró portada en Suno.".to_string())?;
            let fp = download_dir.join(format!("{base_name}.jpg"));
            let tmp = download_dir.join(format!("{base_name}.jpg.tmp"));
            (u, fp, tmp)
        }
        MediaFormat::VideoMp4 => {
            let u = info
                .suno_mp4_url
                .clone()
                .ok_or_else(|| "No se encontró MP4 en Suno.".to_string())?;
            let fp = download_dir.join(format!("{base_name}.mp4"));
            let tmp = download_dir.join(format!("{base_name}.mp4.tmp"));
            (u, fp, tmp)
        }
        MediaFormat::AudioMp3 => {
            let fp = download_dir.join(format!("{base_name}.mp3"));
            if ffmpeg_path.is_some() {
                let u = info
                    .suno_mp4_url
                    .clone()
                    .ok_or_else(|| "No se encontró flujo de audio en Suno.".to_string())?;
                let tmp = download_dir.join(format!("{base_name}.mp3.tmp.mp4"));
                (u, fp, tmp)
            } else {
                let u = format!("https://cdn1.suno.ai/{song_id}.mp3");
                let tmp = download_dir.join(format!("{base_name}.mp3.tmp"));
                (u, fp, tmp)
            }
        }
    };

    let _ = tx.send(UiMessage::Log(format!(
        "[INFO] Pista detectada: {}",
        info.title.as_deref().unwrap_or("Suno")
    )));

    let res: Result<(), String> = (|| {
        let client = build_http_client(30)?;
        let mut resp = client
            .get(&target_url)
            .send()
            .map_err(|e| format!("Error descargando Suno: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Suno respondió HTTP {}", resp.status()));
        }
        let total = resp.content_length().or(info.filesize).unwrap_or(0);
        let mut downloaded: u64 = 0;
        let mut buf = vec![0u8; 256 * 1024];
        let mut out = File::create(&temp_download_path)
            .map_err(|e| format!("Error creando archivo temporal: {e}"))?;
        let mut last_tick = Instant::now() - std::time::Duration::from_millis(200);

        loop {
            if cancel.is_cancelled() {
                return Err("Descarga cancelada por el usuario.".into());
            }
            let n = resp
                .read(&mut buf)
                .map_err(|e| format!("Error leyendo flujo Suno: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("Error escribiendo archivo: {e}"))?;
            downloaded += n as u64;
            if total > 0 && last_tick.elapsed().as_millis() >= 60 {
                last_tick = Instant::now();
                let scale = if selected_format == MediaFormat::AudioMp3 && ffmpeg_path.is_some() {
                    0.90
                } else {
                    0.98
                };
                let frac = ((downloaded as f32) / (total as f32)) * scale;
                let _ = tx.send(UiMessage::Status {
                    text: format!("Descargando ({})...", format_bytes(total, true)),
                    progress: Some(frac),
                    percent: Some(format!("{}%", (frac * 100.0) as u32)),
                });
            }
        }
        out.flush().map_err(|e| format!("Error guardando archivo: {e}"))?;
        drop(out);

        if selected_format == MediaFormat::AudioMp3 {
            if let Some(ffmpeg) = ffmpeg_path {
                let kbps = audio_quality.kbps();
                let _ = tx.send(UiMessage::Log(format!(
                    "Convirtiendo pista de audio a MP3 ({kbps} kbps)..."
                )));
                let _ = tx.send(UiMessage::Status {
                    text: format!("Codificando archivo MP3 a {kbps} kbps..."),
                    progress: Some(0.95),
                    percent: Some("95%".into()),
                });
                let mut cmd = Command::new(ffmpeg);
                cmd.args([
                    "-y",
                    "-threads",
                    "0",
                    "-i",
                    &temp_download_path.to_string_lossy(),
                    "-vn",
                    "-acodec",
                    "libmp3lame",
                    "-b:a",
                    &format!("{kbps}k"),
                    &final_path.to_string_lossy(),
                ]);
                if let Err(err) = run_ffmpeg_cancellable(&mut cmd, cancel) {
                    if cancel.is_cancelled() && final_path.is_file() {
                        let _ = fs::remove_file(&final_path);
                    }
                    return Err(err);
                }
                if !final_path.is_file() {
                    return Err("Error al convertir el audio a MP3 con FFmpeg.".into());
                }
                return Ok(());
            }
        }

        safe_replace_file(&temp_download_path, &final_path)
            .map_err(|e| format!("Error moviendo archivo final: {e}"))?;
        Ok(())
    })();

    if temp_download_path.is_file() {
        let _ = fs::remove_file(&temp_download_path);
    }
    res?;
    Ok(final_path)
}

#[allow(clippy::too_many_arguments)]
fn build_ytdlp_download_args(
    target_url: &str,
    output_template: &str,
    dl_format: MediaFormat,
    video_quality: VideoQuality,
    audio_quality: AudioQuality,
    locked_id: Option<&str>,
    ffmpeg_path: Option<&Path>,
    qjs_path: Option<&Path>,
    clip_range: Option<(f64, f64)>,
) -> Vec<String> {
    let mut args = vec![
        "--encoding".to_string(),
        "utf-8".to_string(),
        "--no-playlist".to_string(),
        "--force-overwrites".to_string(),
        "--continue".to_string(),
        "--windows-filenames".to_string(),
        "--retries".to_string(),
        "25".to_string(),
        "--fragment-retries".to_string(),
        "25".to_string(),
        "--socket-timeout".to_string(),
        "30".to_string(),
        "--retry-sleep".to_string(),
        "http:exp=1:15".to_string(),
        "--retry-sleep".to_string(),
        "fragment:exp=1:15".to_string(),
        "--concurrent-fragments".to_string(),
        "4".to_string(),
        "--buffer-size".to_string(),
        "256K".to_string(),
        "--newline".to_string(),
    ];
    args.extend(ytdlp_js_cli_args(qjs_path));

    if let Some((start_sec, end_sec)) = clip_range {
        args.push("--download-sections".into());
        args.push(format!("*{start_sec:.2}-{end_sec:.2}"));
        args.push("--force-keyframes-at-cuts".into());
    }

    if let Some(ff) = ffmpeg_path {
        args.push("--ffmpeg-location".into());
        args.push(ff.to_string_lossy().to_string());
    }

    args.push("-o".into());
    args.push(output_template.to_string());

    match dl_format {
        MediaFormat::VideoMp4 => {
            args.push("-f".into());
            args.push(video_format_selector(
                video_quality,
                locked_id,
                ffmpeg_path.is_some(),
            ));
            if ffmpeg_path.is_some() {
                args.push("--merge-output-format".into());
                args.push("mp4".into());
            }
        }
        MediaFormat::AudioMp3 => {
            args.push("-f".into());
            args.push("bestaudio/best".into());
            if ffmpeg_path.is_some() {
                args.extend([
                    "-x".into(),
                    "--audio-format".into(),
                    "mp3".into(),
                    "--audio-quality".into(),
                    format!("{}K", audio_quality.kbps()),
                ]);
            }
        }
        MediaFormat::ImageGif => {
            args.push("-f".into());
            args.push("best".into());
        }
    }

    args.push(target_url.to_string());
    args
}

#[allow(clippy::too_many_arguments)]
fn download_twitter_fallback(
    url: &str,
    cached_info: Option<&MediaInfo>,
    selected_format: MediaFormat,
    video_quality: VideoQuality,
    audio_quality: AudioQuality,
    locked_id: Option<&str>,
    clip_range: Option<(f64, f64)>,
    download_dir: &Path,
    ytdlp_path: &Path,
    ffmpeg_path: Option<&Path>,
    qjs_path: Option<&Path>,
    tx: &Sender<UiMessage>,
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, String> {
    let fb_info = match cached_info {
        Some(i) if i.tw_direct_fallback => i.clone(),
        _ => extract_twitter_fallback_info(url)?
            .ok_or_else(|| "No se pudo extraer el video mediante el respaldo de X/Twitter.".to_string())?,
    };

    if selected_format == MediaFormat::ImageGif && !fb_info.tw_photo_urls.is_empty() {
        return download_static_urls(&fb_info.tw_photo_urls, fb_info.id.as_deref().unwrap_or("x"), download_dir, tx);
    }

    let direct_url = select_twitter_fallback_stream_url(
        &fb_info,
        video_quality,
        locked_id,
        ffmpeg_path.is_some(),
    )
    .ok_or_else(|| "No se encontró flujo MP4/HLS en la publicación de X.".to_string())?;

    let _ = tx.send(UiMessage::Log(
        "[INFO] Usando extracción directa de CDN para publicación restringida de X/Twitter...".into(),
    ));
    fs::create_dir_all(download_dir).map_err(|e| format!("Error creando carpeta: {e}"))?;

    let safe_title = sanitize_filename(fb_info.title.as_deref().unwrap_or("Video_X"), 95);
    let vid_id = fb_info.id.as_deref().unwrap_or("x");
    let clip_part = if let Some((s, e)) = clip_range {
        let ts = format_timestamp_input(s).replace(':', "m") + "s";
        let te = format_timestamp_input(e).replace(':', "m") + "s";
        format!(" [Clip {ts}-{te}]")
    } else {
        String::new()
    };

    let (tmp_suffix, dl_format) = match selected_format {
        MediaFormat::ImageGif => (".gif_tmp", MediaFormat::VideoMp4),
        MediaFormat::AudioMp3 => (".audio_tmp", MediaFormat::AudioMp3),
        MediaFormat::VideoMp4 => ("", MediaFormat::VideoMp4),
    };

    let out_tmpl = download_dir
        .join(format!("{safe_title} [{vid_id}]{clip_part}{tmp_suffix}.%(ext)s"))
        .to_string_lossy()
        .to_string();

    let mut args = build_ytdlp_download_args(
        &direct_url,
        &out_tmpl,
        dl_format,
        video_quality,
        audio_quality,
        None,
        ffmpeg_path,
        qjs_path,
        clip_range,
    );
    // Reemplazar -f por best para descarga directa desde URL de CDN
    if let Some(pos) = args.iter().position(|a| a == "-f") {
        if pos + 1 < args.len() {
            args[pos + 1] = "best".into();
        }
    }

    let mut downloaded = run_ytdlp_cli(
        ytdlp_path,
        &args,
        download_dir,
        if clip_range.is_some() { "Recortando" } else { "Descargando" },
        tx,
        cancel,
    )?;

    if selected_format == MediaFormat::AudioMp3 {
        if let Some(ref p) = downloaded {
            let s = p.to_string_lossy();
            if s.contains(".audio_tmp.") && p.is_file() {
                let clean = PathBuf::from(s.replace(".audio_tmp.", "."));
                if safe_replace_file(p, &clean).is_ok() {
                    downloaded = Some(clean);
                }
            }
        }
    }

    Ok(downloaded)
}

fn download_static_urls(
    urls: &[String],
    prefix: &str,
    download_dir: &Path,
    tx: &Sender<UiMessage>,
) -> Result<Option<PathBuf>, String> {
    fs::create_dir_all(download_dir).map_err(|e| format!("Error creando carpeta: {e}"))?;
    let client = build_http_client(15)?;
    let mut last_saved = None;
    for (i, u) in urls.iter().enumerate() {
        let resp = client.get(u).send().map_err(|e| format!("Error descargando imagen: {e}"))?;
        if !resp.status().is_success() {
            continue;
        }
        let ct = resp
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let ext = if u.contains(".png") || ct.contains("png") {
            "png"
        } else if u.contains(".webp") || ct.contains("webp") {
            "webp"
        } else {
            "jpg"
        };
        let target = download_dir.join(format!("Imagen_{prefix}_{}.{ext}", i + 1));
        let bytes = resp.bytes().map_err(|e| format!("Error leyendo imagen: {e}"))?;
        fs::write(&target, &bytes).map_err(|e| format!("Error guardando imagen: {e}"))?;
        let _ = tx.send(UiMessage::Log(format!(
            "[ARCHIVO] Imagen guardada: {}",
            target.file_name().and_then(|s| s.to_str()).unwrap_or("")
        )));
        last_saved = Some(target);
    }
    Ok(last_saved)
}

fn fallback_static_image_html(
    url: &str,
    download_dir: &Path,
    tx: &Sender<UiMessage>,
) -> Option<PathBuf> {
    if is_twitter_status_url(url) {
        if let Ok(Some(fb)) = extract_twitter_fallback_info(url) {
            if !fb.tw_photo_urls.is_empty() {
                if let Ok(res) = download_static_urls(
                    &fb.tw_photo_urls,
                    fb.id.as_deref().unwrap_or("x"),
                    download_dir,
                    tx,
                ) {
                    return res;
                }
            }
        }
    }
    let client = build_http_client(10).ok()?;
    let html = client.get(url).send().ok()?.text().ok()?;
    static OG_RE: OnceLock<Regex> = OnceLock::new();
    let re = OG_RE.get_or_init(|| {
        Regex::new(r#"(?i)<meta[^>]+(?:property|name)=["'](?:og:image|twitter:image)["'][^>]+content=["']([^"']+)["']"#).unwrap()
    });
    let img_url = re.captures(&html)?.get(1)?.as_str().to_string();
    download_static_urls(&[img_url], "web", download_dir, tx).ok().flatten()
}

#[allow(clippy::too_many_arguments)]
pub fn execute_single_download(
    url: &str,
    cached_info: Option<&MediaInfo>,
    mut selected_format: MediaFormat,
    video_quality: VideoQuality,
    audio_quality: AudioQuality,
    locked_id: Option<&str>,
    download_dir: &Path,
    ytdlp_path: &Path,
    ffmpeg_path: Option<&Path>,
    qjs_path: Option<&Path>,
    tx: &Sender<UiMessage>,
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, String> {
    if is_suno_url(url) {
        let p = download_suno_media(
            url,
            cached_info,
            selected_format,
            audio_quality,
            download_dir,
            ffmpeg_path,
            tx,
            cancel,
        )?;
        let _ = tx.send(UiMessage::Log(format!(
            "[ARCHIVO] Guardado como: {}",
            p.file_name().and_then(|s| s.to_str()).unwrap_or("")
        )));
        return Ok(Some(p));
    }

    if let Some(info) = cached_info {
        if is_twitter_gif_info(info) {
            selected_format = MediaFormat::ImageGif;
        }
    }

    let is_tw_fb = cached_info.map(|i| i.tw_direct_fallback).unwrap_or(false);
    let mut downloaded_filepath: Option<PathBuf>;

    if is_tw_fb {
        downloaded_filepath = download_twitter_fallback(
            url,
            cached_info,
            selected_format,
            video_quality,
            audio_quality,
            locked_id,
            None,
            download_dir,
            ytdlp_path,
            ffmpeg_path,
            qjs_path,
            tx,
            cancel,
        )?;
    } else {
        let out_tmpl = download_dir
            .join("%(title).120B [%(id)s].%(ext)s")
            .to_string_lossy()
            .to_string();
        let args = build_ytdlp_download_args(
            url,
            &out_tmpl,
            selected_format,
            video_quality,
            audio_quality,
            locked_id,
            ffmpeg_path,
            qjs_path,
            None,
        );
        match run_ytdlp_cli(ytdlp_path, &args, download_dir, "Descargando", tx, cancel) {
            Ok(fp) => downloaded_filepath = fp,
            Err(primary_err) => {
                if cancel.is_cancelled() {
                    return Err(primary_err);
                }
                if primary_err.contains("429") || primary_err.contains("Too Many Requests") {
                    return Err(
                        "El servidor limitó temporalmente las peticiones (HTTP 429: Too Many Requests). \
                         Espera 1 o 2 minutos y vuelve a pulsar 'INICIAR DESCARGA' para reanudar desde donde quedó."
                            .into(),
                    );
                }
                if is_twitter_status_url(url) {
                    let _ = tx.send(UiMessage::Log(
                        "[AVISO] Falló la descarga principal de X/Twitter; intentando respaldo de CDN..."
                            .into(),
                    ));
                    downloaded_filepath = download_twitter_fallback(
                        url,
                        cached_info,
                        selected_format,
                        video_quality,
                        audio_quality,
                        locked_id,
                        None,
                        download_dir,
                        ytdlp_path,
                        ffmpeg_path,
                        qjs_path,
                        tx,
                        cancel,
                    )?;
                } else if selected_format == MediaFormat::ImageGif {
                    if let Some(img_path) = fallback_static_image_html(url, download_dir, tx) {
                        return Ok(Some(img_path));
                    }
                    return Err(primary_err);
                } else {
                    return Err(primary_err);
                }
            }
        }
    }

    if selected_format == MediaFormat::ImageGif {
        if let Some(ref p) = downloaded_filepath {
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(ext.as_str(), "mp4" | "webm" | "mkv") {
                let gif_p = convert_video_to_gif(ffmpeg_path, p, None, 15, tx, cancel)?;
                let _ = tx.send(UiMessage::Log(format!(
                    "GIF generado exitosamente: {}",
                    gif_p.file_name().and_then(|s| s.to_str()).unwrap_or("")
                )));
                downloaded_filepath = Some(gif_p);
            }
        }
    }

    Ok(downloaded_filepath)
}

#[allow(clippy::too_many_arguments)]
fn trim_local_media_with_ffmpeg(
    ffmpeg_path: Option<&Path>,
    input_path: &Path,
    output_path: &Path,
    start_sec: f64,
    end_sec: f64,
    dl_format: MediaFormat,
    audio_quality: AudioQuality,
    cancel: &CancelToken,
) -> Result<PathBuf, String> {
    let ffmpeg =
        ffmpeg_path.ok_or_else(|| "FFmpeg es necesario para recortar fragmentos.".to_string())?;
    let duration = (end_sec - start_sec).max(0.5);
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-y",
        "-ss",
        &format!("{start_sec:.2}"),
        "-i",
        &input_path.to_string_lossy(),
        "-t",
        &format!("{duration:.2}"),
    ]);
    if dl_format == MediaFormat::AudioMp3 {
        cmd.args([
            "-vn",
            "-acodec",
            "libmp3lame",
            "-b:a",
            &format!("{}k", audio_quality.kbps()),
            &output_path.to_string_lossy(),
        ]);
    } else {
        cmd.args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "18",
            "-c:a",
            "aac",
            &output_path.to_string_lossy(),
        ]);
    }
    run_ffmpeg_cancellable(&mut cmd, cancel)?;
    if !output_path.is_file() {
        return Err("No se pudo recortar el segmento con FFmpeg.".into());
    }
    Ok(output_path.to_path_buf())
}

#[allow(clippy::too_many_arguments)]
pub fn execute_clip_download(
    url: &str,
    cached_info: Option<&MediaInfo>,
    start_sec: f64,
    end_sec: f64,
    selected_format: MediaFormat,
    video_quality: VideoQuality,
    audio_quality: AudioQuality,
    gif_quality: GifQuality,
    locked_id: Option<&str>,
    download_dir: &Path,
    ytdlp_path: &Path,
    ffmpeg_path: Option<&Path>,
    qjs_path: Option<&Path>,
    tx: &Sender<UiMessage>,
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, String> {
    fs::create_dir_all(download_dir).map_err(|e| format!("Error creando carpeta: {e}"))?;
    let tag_start = format_timestamp_input(start_sec).replace(':', "m") + "s";
    let tag_end = format_timestamp_input(end_sec).replace(':', "m") + "s";
    let clip_tag = format!("Clip {tag_start}-{tag_end}");

    let (dl_format, dl_video_quality) = if selected_format == MediaFormat::ImageGif {
        (MediaFormat::VideoMp4, VideoQuality::Hd720)
    } else {
        (selected_format, video_quality)
    };

    let mut downloaded_filepath: Option<PathBuf>;

    if is_suno_url(url) {
        let raw_path = download_suno_media(
            url,
            cached_info,
            dl_format,
            audio_quality,
            download_dir,
            ffmpeg_path,
            tx,
            cancel,
        )?;
        let stem = raw_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Suno");
        let clipped_ext = match selected_format {
            MediaFormat::AudioMp3 => "mp3",
            MediaFormat::ImageGif => "gif_tmp.mp4",
            MediaFormat::VideoMp4 => "mp4",
        };
        let clipped_path = raw_path.with_file_name(format!("{stem} [{clip_tag}].{clipped_ext}"));
        let _ = tx.send(UiMessage::Status {
            text: "Recortando segmento exacto con FFmpeg...".into(),
            progress: Some(0.92),
            percent: Some("92%".into()),
        });
        let trimmed = trim_local_media_with_ffmpeg(
            ffmpeg_path,
            &raw_path,
            &clipped_path,
            start_sec,
            end_sec,
            dl_format,
            audio_quality,
            cancel,
        )?;
        if raw_path.is_file() && raw_path != trimmed {
            let _ = fs::remove_file(&raw_path);
        }
        downloaded_filepath = Some(trimmed);
    } else if cached_info.map(|i| i.tw_direct_fallback).unwrap_or(false) {
        downloaded_filepath = download_twitter_fallback(
            url,
            cached_info,
            selected_format,
            dl_video_quality,
            audio_quality,
            locked_id,
            Some((start_sec, end_sec)),
            download_dir,
            ytdlp_path,
            ffmpeg_path,
            qjs_path,
            tx,
            cancel,
        )?;
    } else {
        let tmp_suffix = match selected_format {
            MediaFormat::ImageGif => ".gif_tmp",
            MediaFormat::AudioMp3 => ".audio_tmp",
            MediaFormat::VideoMp4 => "",
        };
        let out_tmpl = download_dir
            .join(format!(
                "%(title).100B [%(id)s] [{clip_tag}]{tmp_suffix}.%(ext)s"
            ))
            .to_string_lossy()
            .to_string();
        let args = build_ytdlp_download_args(
            url,
            &out_tmpl,
            dl_format,
            dl_video_quality,
            audio_quality,
            if dl_format == MediaFormat::VideoMp4 {
                locked_id
            } else {
                None
            },
            ffmpeg_path,
            qjs_path,
            Some((start_sec, end_sec)),
        );
        match run_ytdlp_cli(ytdlp_path, &args, download_dir, "Recortando", tx, cancel) {
            Ok(mut fp) => {
                if selected_format == MediaFormat::AudioMp3 {
                    if let Some(ref p) = fp {
                        let s = p.to_string_lossy();
                        if s.contains(".audio_tmp.") && p.is_file() {
                            let clean = PathBuf::from(s.replace(".audio_tmp.", "."));
                            if safe_replace_file(p, &clean).is_ok() {
                                fp = Some(clean);
                            }
                        }
                    }
                }
                downloaded_filepath = fp;
            }
            Err(primary_err) => {
                if cancel.is_cancelled() {
                    return Err(primary_err);
                }
                if is_twitter_status_url(url) {
                    downloaded_filepath = download_twitter_fallback(
                        url,
                        cached_info,
                        selected_format,
                        dl_video_quality,
                        audio_quality,
                        locked_id,
                        Some((start_sec, end_sec)),
                        download_dir,
                        ytdlp_path,
                        ffmpeg_path,
                        qjs_path,
                        tx,
                        cancel,
                    )?;
                } else {
                    return Err(primary_err);
                }
            }
        }
    }

    if selected_format == MediaFormat::ImageGif {
        if let Some(ref p) = downloaded_filepath {
            if p.is_file() {
                let (max_h, fps) = gif_quality.params();
                let gif_p = convert_video_to_gif(ffmpeg_path, p, max_h, fps, tx, cancel)?;
                let _ = tx.send(UiMessage::Log(format!(
                    "Fragmento GIF generado exitosamente: {}",
                    gif_p.file_name().and_then(|s| s.to_str()).unwrap_or("")
                )));
                downloaded_filepath = Some(gif_p);
            }
        }
    }

    Ok(downloaded_filepath)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_replace_file_overwrites_existing() {
        let dir = std::env::temp_dir().join("ud_rust_replace_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let src = dir.join("clip.audio_tmp.mp3");
        let dst = dir.join("clip.mp3");
        fs::write(&dst, b"old-content").expect("write old");
        fs::write(&src, b"new-content").expect("write new");

        safe_replace_file(&src, &dst).expect("replace should succeed");
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).expect("read dst"), b"new-content");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ffmpeg_large_stderr_no_deadlock() {
        let ffmpeg = crate::config::get_appdata_bin_dir().join("ffmpeg.exe");
        if !ffmpeg.is_file() {
            return;
        }
        let dir = std::env::temp_dir().join("ud_rust_ffmpeg_pipe_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let sample = dir.join("sample.mp4");
        let trimmed = dir.join("trimmed.mp4");
        let cancel = CancelToken::new();

        let mut gen_cmd = Command::new(&ffmpeg);
        gen_cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "mandelbrot=size=1280x720:rate=30",
            "-t",
            "20",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            &sample.to_string_lossy(),
        ]);
        run_ffmpeg_cancellable(&mut gen_cmd, &cancel).expect("generate sample mp4");

        let mut trim_cmd = Command::new(&ffmpeg);
        trim_cmd.args([
            "-y",
            "-ss",
            "0.00",
            "-i",
            &sample.to_string_lossy(),
            "-t",
            "20.00",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "18",
            "-an",
            &trimmed.to_string_lossy(),
        ]);
        run_ffmpeg_cancellable(&mut trim_cmd, &cancel).expect("trim without pipe deadlock");
        assert!(trimmed.is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
