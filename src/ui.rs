use crate::config::{default_downloads_dir, get_appdata_bin_dir, load_config, save_config, AppConfig};
use crate::downloader::{execute_clip_download, execute_single_download, CancelToken};
use crate::engines::{
    apply_no_window, detect_local_ytdlp_version, download_ytdlp_update, ensure_bundled_engines,
    fetch_latest_remote_version, is_newer_version,
};
use crate::extractors::{
    analyze_url, download_thumbnail_pixels, extract_best_thumbnail_url, select_optimal_video_format,
};
use crate::models::{
    clean_ansi, format_bytes, format_duration, format_timestamp_input, has_video,
    is_twitter_gif_info, parse_timestamp_input, resolution_label, stream_bytes, ActiveTab,
    AudioQuality, GifQuality, MediaFormat, MediaInfo, PlaylistRowState, ThemeMode, UiMessage,
    VideoQuality, APP_VERSION,
};
use crate::theme::{apply_theme, ThemePalette};
use egui::{
    Align, Color32, ColorImage, CornerRadius, Frame, Layout, Margin, RichText, Stroke,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

struct LruCache<V> {
    cap: usize,
    order: VecDeque<String>,
    map: HashMap<String, V>,
}

impl<V: Clone> LruCache<V> {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            order: VecDeque::with_capacity(cap),
            map: HashMap::with_capacity(cap),
        }
    }

    fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    fn insert(&mut self, key: String, val: V) {
        if self.map.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else if self.map.len() >= self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, val);
    }
}

pub struct UniversalDownloaderApp {
    tx: Sender<UiMessage>,
    rx: Receiver<UiMessage>,

    config: AppConfig,
    theme_mode: ThemeMode,
    palette: ThemePalette,
    active_tab: ActiveTab,

    download_dir: PathBuf,
    ytdlp_path: PathBuf,
    ffmpeg_path: Option<PathBuf>,
    qjs_path: Option<PathBuf>,
    engines_ready: bool,
    engine_version: String,

    is_busy: bool,
    busy_action_text: String,
    cancel_token: CancelToken,

    update_available_text: Option<String>,

    // Estado Pestaña 1 (Descarga General / Listas)
    main_url: String,
    main_last_edited_url: String,
    main_debounce_deadline: Option<Instant>,
    main_analyzed_url: Option<String>,
    main_analyzed_info: Option<MediaInfo>,
    main_locked_format_id: Option<String>,
    main_analyze_gen: u64,
    main_format: MediaFormat,
    main_video_quality: VideoQuality,
    main_audio_quality: AudioQuality,
    main_media_info_text: String,
    main_size_label: String,
    main_thumb_tex: Option<TextureHandle>,
    is_playlist_mode: bool,
    playlist_rows: Vec<PlaylistRowState>,

    // Estado Pestaña 2 (Recortar Fragmento / Clip)
    clip_url: String,
    clip_last_edited_url: String,
    clip_debounce_deadline: Option<Instant>,
    clip_analyzed_url: Option<String>,
    clip_analyzed_info: Option<MediaInfo>,
    clip_locked_format_id: Option<String>,
    clip_analyze_gen: u64,
    clip_total_duration: f64,
    clip_start_sec: f64,
    clip_end_sec: f64,
    clip_start_text: String,
    clip_end_text: String,
    clip_format: MediaFormat,
    clip_video_quality: VideoQuality,
    clip_audio_quality: AudioQuality,
    clip_gif_quality: GifQuality,
    clip_media_info_text: String,
    clip_size_label: String,
    clip_thumb_tex: Option<TextureHandle>,
    clip_loading: bool,

    // Estado inferior (Progreso y Consola)
    status_text: String,
    progress: f32,
    percent_text: String,
    logs: VecDeque<String>,

    // Cachés LRU en memoria (32 URLs y 32 miniaturas)
    url_cache: LruCache<MediaInfo>,
    thumb_cache: LruCache<TextureHandle>,
    placeholder_tex: Option<TextureHandle>,
}

impl UniversalDownloaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel();
        let config = load_config();
        let theme_mode = config
            .theme
            .as_deref()
            .map(ThemeMode::from_str_saved)
            .unwrap_or(ThemeMode::Dark);
        let palette = apply_theme(&cc.egui_ctx, theme_mode);

        let download_dir = config
            .download_dir
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(default_downloads_dir);

        let bin_dir = get_appdata_bin_dir();
        let ytdlp_path = bin_dir.join("yt-dlp.exe");
        let ffmpeg_path = bin_dir.join("ffmpeg.exe");
        let qjs_path = bin_dir.join("qjs.exe");
        let engines_already_present =
            ytdlp_path.is_file() && ffmpeg_path.is_file() && qjs_path.is_file();

        let placeholder_tex = Some(Self::create_placeholder_texture(&cc.egui_ctx));

        let mut logs = VecDeque::with_capacity(500);
        logs.push_back(format!(
            "Universal Downloader v{APP_VERSION} (Rust nativo) iniciado correctamente."
        ));
        logs.push_back(
            "Plataformas activas: YouTube, TikTok, Instagram, Facebook y Twitter (X).".into(),
        );

        let app = Self {
            tx: tx.clone(),
            rx,
            config: config.clone(),
            theme_mode,
            palette,
            active_tab: ActiveTab::Main,
            download_dir,
            ytdlp_path,
            ffmpeg_path: ffmpeg_path.is_file().then_some(ffmpeg_path),
            qjs_path: qjs_path.is_file().then_some(qjs_path),
            engines_ready: false,
            engine_version: config
                .ext_engine_version
                .clone()
                .unwrap_or_else(|| "...".into()),
            is_busy: false,
            busy_action_text: "PROCESANDO DESCARGA...".into(),
            cancel_token: CancelToken::new(),
            update_available_text: None,

            main_url: String::new(),
            main_last_edited_url: String::new(),
            main_debounce_deadline: None,
            main_analyzed_url: None,
            main_analyzed_info: None,
            main_locked_format_id: None,
            main_analyze_gen: 0,
            main_format: MediaFormat::VideoMp4,
            main_video_quality: VideoQuality::BestAuto,
            main_audio_quality: AudioQuality::Kbps320,
            main_media_info_text: "Pega un enlace para calcular calidades y peso automáticamente"
                .into(),
            main_size_label: "Peso estimado: Pega un enlace".into(),
            main_thumb_tex: None,
            is_playlist_mode: false,
            playlist_rows: Vec::new(),

            clip_url: String::new(),
            clip_last_edited_url: String::new(),
            clip_debounce_deadline: None,
            clip_analyzed_url: None,
            clip_analyzed_info: None,
            clip_locked_format_id: None,
            clip_analyze_gen: 0,
            clip_total_duration: 0.0,
            clip_start_sec: 0.0,
            clip_end_sec: 15.0,
            clip_start_text: "00:00".into(),
            clip_end_text: "00:15".into(),
            clip_format: MediaFormat::VideoMp4,
            clip_video_quality: VideoQuality::BestAuto,
            clip_audio_quality: AudioQuality::Kbps320,
            clip_gif_quality: GifQuality::High480,
            clip_media_info_text: "Pega un enlace para cargar la duración y elegir el fragmento"
                .into(),
            clip_size_label: "Peso del fragmento: Pega un enlace".into(),
            clip_thumb_tex: None,
            clip_loading: false,

            status_text: if engines_already_present {
                "Listo para descargar".into()
            } else {
                "Preparando motores integrados (primera ejecución)...".into()
            },
            progress: 0.0,
            percent_text: "0%".into(),
            logs,

            url_cache: LruCache::new(32),
            thumb_cache: LruCache::new(32),
            placeholder_tex,
        };

        // Hilo de inicialización: extrae motores si faltan y consulta actualizaciones en silencio
        let ctx_clone = cc.egui_ctx.clone();
        let mut cfg_bg = config;
        std::thread::spawn(move || {
            let paths = ensure_bundled_engines(|msg| {
                let _ = tx.send(UiMessage::Log(msg));
            });
            let ver = detect_local_ytdlp_version(&paths.ytdlp, &mut cfg_bg);
            let _ = tx.send(UiMessage::EnginesReady {
                ytdlp_path: paths.ytdlp,
                ffmpeg_path: paths.ffmpeg,
                qjs_path: paths.qjs,
                engine_version: ver.clone(),
                config: cfg_bg,
            });
            if let Some(latest) = fetch_latest_remote_version() {
                if is_newer_version(&latest, &ver) {
                    let _ = tx.send(UiMessage::UpdateAvailable {
                        local_ver: ver,
                        latest_ver: latest,
                    });
                } else {
                    let _ = tx.send(UiMessage::Log(format!(
                        "[OK] API de descargas al día (v{ver}). No se requieren actualizaciones."
                    )));
                }
            }
            ctx_clone.request_repaint();
        });

        app
    }

    fn create_placeholder_texture(ctx: &egui::Context) -> TextureHandle {
        let mut rgba = vec![0u8; 88 * 50 * 4];
        for y in 0..50usize {
            for x in 0..88usize {
                let idx = (y * 88 + x) * 4;
                let border = x == 0 || y == 0 || x == 87 || y == 49;
                if border {
                    rgba[idx] = 0x35;
                    rgba[idx + 1] = 0x3b;
                    rgba[idx + 2] = 0x48;
                } else {
                    rgba[idx] = 0x1e;
                    rgba[idx + 1] = 0x22;
                    rgba[idx + 2] = 0x2a;
                }
                rgba[idx + 3] = 0xff;
            }
        }
        let img = ColorImage::from_rgba_unmultiplied([88, 50], &rgba);
        ctx.load_texture("thumb_placeholder", img, TextureOptions::LINEAR)
    }

    fn push_log(&mut self, msg: impl Into<String>) {
        if self.logs.len() >= 500 {
            self.logs.pop_front();
        }
        self.logs.push_back(msg.into());
    }

    fn spawn_thumb_fetch(
        &self,
        ctx: &egui::Context,
        info: &MediaInfo,
        generation: u64,
        is_clip_tab: bool,
    ) {
        let Some(thumb_url) = extract_best_thumbnail_url(info) else {
            return;
        };
        if self.thumb_cache.get(&thumb_url).is_some() {
            return;
        }
        let tx = self.tx.clone();
        let ctx_c = ctx.clone();
        std::thread::spawn(move || {
            if let Ok(pixels) = download_thumbnail_pixels(&thumb_url) {
                let _ = tx.send(UiMessage::ThumbnailLoaded {
                    is_clip_tab,
                    generation,
                    pixels,
                });
                ctx_c.request_repaint();
            }
        });
    }

    fn poll_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMessage::Log(line) => self.push_log(line),
                UiMessage::Status {
                    text,
                    progress,
                    percent,
                } => {
                    self.status_text = text;
                    if let Some(p) = progress {
                        self.progress = p.clamp(0.0, 1.0);
                    }
                    if let Some(pct) = percent {
                        self.percent_text = pct;
                    }
                }
                UiMessage::Busy { busy, action_text } => {
                    self.is_busy = busy;
                    if let Some(t) = action_text {
                        self.busy_action_text = t;
                    }
                }
                UiMessage::EnginesReady {
                    ytdlp_path,
                    ffmpeg_path,
                    qjs_path,
                    engine_version,
                    config,
                } => {
                    self.ytdlp_path = ytdlp_path;
                    self.ffmpeg_path = ffmpeg_path;
                    self.qjs_path = qjs_path;
                    self.engine_version = engine_version.clone();
                    self.config.ext_engine_mtime = config.ext_engine_mtime;
                    self.config.ext_engine_version = config.ext_engine_version;
                    self.engines_ready = true;
                    if self.status_text.starts_with("Preparando motores") {
                        self.status_text = "Listo para descargar".into();
                    }
                    if let Some(ref ff) = self.ffmpeg_path {
                        let name = ff.file_name().and_then(|s| s.to_str()).unwrap_or("ffmpeg");
                        self.push_log(format!("Motor FFmpeg integrado listo: {name}"));
                    }
                    if let Some(ref q) = self.qjs_path {
                        let name = q.file_name().and_then(|s| s.to_str()).unwrap_or("qjs");
                        self.push_log(format!("Motor JS portable (QuickJS fallback) listo: {name}"));
                    }
                    self.push_log(format!("Motor de extracción activo: v{engine_version}"));
                }
                UiMessage::UpdateAvailable {
                    local_ver,
                    latest_ver,
                } => {
                    let msg = format!(
                        "¡Nueva actualización de API disponible! (Actual: {local_ver} -> Nueva: {latest_ver})"
                    );
                    self.push_log(format!(
                        "[ACTUALIZACIÓN] {msg}. Pulsa el botón 'Actualizar API' para instalarla."
                    ));
                    self.status_text = msg;
                    self.percent_text = "Update".into();
                    self.update_available_text = Some("¡Actualizar API!".into());
                }
                UiMessage::UpdateFinished {
                    engine_version,
                    config,
                } => {
                    self.update_available_text = None;
                    self.engine_version = engine_version;
                    self.config.ext_engine_mtime = config.ext_engine_mtime;
                    self.config.ext_engine_version = config.ext_engine_version;
                }
                UiMessage::MainAnalyzed {
                    generation,
                    url,
                    info,
                } => {
                    if generation != self.main_analyze_gen {
                        continue;
                    }
                    self.url_cache.insert(url.clone(), info.clone());
                    self.apply_main_analyzed_info(ctx, url, info, generation);
                }
                UiMessage::MainAnalyzeFailed { generation, error } => {
                    if generation == self.main_analyze_gen {
                        self.main_media_info_text = "Enlace listo para descargar".into();
                        self.main_size_label = "Peso estimado: Variable según plataforma".into();
                        let short_err: String = clean_ansi(&error).chars().take(220).collect();
                        if !short_err.trim().is_empty() {
                            self.push_log(format!(
                                "[AVISO] No se pudo pre-analizar el enlace: {short_err}"
                            ));
                        }
                    }
                }
                UiMessage::ClipAnalyzed {
                    generation,
                    url,
                    info,
                } => {
                    if generation != self.clip_analyze_gen {
                        continue;
                    }
                    self.clip_loading = false;
                    self.url_cache.insert(url.clone(), info.clone());
                    self.apply_clip_analyzed_info(ctx, url, info, generation);
                }
                UiMessage::ClipAnalyzeFailed { generation, error } => {
                    if generation == self.clip_analyze_gen {
                        self.clip_loading = false;
                        self.clip_size_label =
                            "Peso del fragmento: No disponible para este enlace".into();
                        self.clip_media_info_text =
                            "No se pudo pre-analizar la duración del enlace".into();
                        let short_err: String = clean_ansi(&error).chars().take(220).collect();
                        if !short_err.trim().is_empty() {
                            self.push_log(format!(
                                "[AVISO] No se pudo pre-analizar el enlace: {short_err}"
                            ));
                        }
                    }
                }
                UiMessage::ThumbnailLoaded {
                    is_clip_tab,
                    generation,
                    pixels,
                } => {
                    let img = ColorImage::from_rgba_unmultiplied(
                        [pixels.width, pixels.height],
                        &pixels.rgba,
                    );
                    let tex = ctx.load_texture(
                        format!("thumb_{}", pixels.url),
                        img,
                        TextureOptions::LINEAR,
                    );
                    self.thumb_cache.insert(pixels.url, tex.clone());
                    if is_clip_tab && generation == self.clip_analyze_gen {
                        self.clip_thumb_tex = Some(tex);
                    } else if !is_clip_tab && generation == self.main_analyze_gen {
                        self.main_thumb_tex = Some(tex);
                    }
                }
                UiMessage::DownloadFinished {
                    final_status,
                    final_percent,
                    progress,
                } => {
                    self.is_busy = false;
                    self.status_text = final_status;
                    self.percent_text = final_percent;
                    self.progress = progress;
                }
            }
        }
    }

    fn apply_main_analyzed_info(
        &mut self,
        ctx: &egui::Context,
        url: String,
        info: MediaInfo,
        generation: u64,
    ) {
        self.main_analyzed_url = Some(url);
        if let Some(turl) = extract_best_thumbnail_url(&info) {
            if let Some(tex) = self.thumb_cache.get(&turl) {
                self.main_thumb_tex = Some(tex.clone());
            } else {
                self.main_thumb_tex = self.placeholder_tex.clone();
                self.spawn_thumb_fetch(ctx, &info, generation, false);
            }
        } else {
            self.main_thumb_tex = self.placeholder_tex.clone();
        }

        if info.is_playlist && info.normalized_entries.len() > 1 {
            let count = info.normalized_entries.len();
            let list_title = info
                .title
                .as_deref()
                .unwrap_or("Lista de reproducción")
                .trim();
            let short_title: String = list_title.chars().take(48).collect();
            self.main_media_info_text = format!("Lista detectada: {short_title} ({count} videos)");
            self.is_playlist_mode = true;
            let default_fmt = match self.main_format {
                MediaFormat::AudioMp3 => MediaFormat::AudioMp3,
                _ => MediaFormat::VideoMp4,
            };
            self.playlist_rows = info
                .normalized_entries
                .iter()
                .enumerate()
                .map(|(i, e)| PlaylistRowState {
                    checked: true,
                    index: i + 1,
                    title: e
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("Elemento #{}", i + 1)),
                    url: e.url.clone().unwrap_or_default(),
                    duration: e.duration,
                    format: default_fmt,
                    video_quality: self.main_video_quality,
                    audio_quality: self.main_audio_quality,
                })
                .collect();
            self.main_analyzed_info = Some(info);
            self.recalculate_main_size();
            return;
        }

        self.is_playlist_mode = false;
        self.playlist_rows.clear();

        let is_tw_gif = is_twitter_gif_info(&info);
        if is_tw_gif && self.main_format != MediaFormat::ImageGif {
            self.main_format = MediaFormat::ImageGif;
        }

        let raw_title = info
            .title
            .as_deref()
            .unwrap_or("Contenido multimedia")
            .trim();
        let dur_str = if is_tw_gif {
            "GIF animado".to_string()
        } else {
            format_duration(info.duration)
        };
        let short_t: String = raw_title.chars().take(46).collect();
        self.main_media_info_text = format!("{short_t}  ({dur_str})");
        self.main_analyzed_info = Some(info);
        self.recalculate_main_size();
    }

    fn apply_clip_analyzed_info(
        &mut self,
        ctx: &egui::Context,
        url: String,
        info: MediaInfo,
        generation: u64,
    ) {
        self.clip_analyzed_url = Some(url);
        let duration = info.duration.unwrap_or(0.0);
        self.clip_total_duration = duration;
        let title = info
            .title
            .as_deref()
            .unwrap_or("Video detectado")
            .trim();
        let short_title: String = title.chars().take(52).collect();
        let dur_str = format_duration(info.duration);
        self.clip_media_info_text = if duration > 0.0 {
            format!("{short_title} ({dur_str})")
        } else {
            short_title
        };

        if let Some(turl) = extract_best_thumbnail_url(&info) {
            if let Some(tex) = self.thumb_cache.get(&turl) {
                self.clip_thumb_tex = Some(tex.clone());
            } else {
                self.clip_thumb_tex = self.placeholder_tex.clone();
                self.spawn_thumb_fetch(ctx, &info, generation, true);
            }
        } else {
            self.clip_thumb_tex = self.placeholder_tex.clone();
        }

        let default_end = if duration > 0.0 { duration } else { 15.0 };
        let has_custom = (self.clip_start_sec > 0.0
            || ((self.clip_end_sec - 10.0).abs() > 0.1 && (self.clip_end_sec - 15.0).abs() > 0.1))
            && self.clip_end_sec > self.clip_start_sec
            && (duration <= 0.0 || self.clip_end_sec <= duration);

        if !has_custom {
            self.clip_start_sec = 0.0;
            self.clip_end_sec = default_end.max(1.0);
        }
        self.clip_start_text = format_timestamp_input(self.clip_start_sec);
        self.clip_end_text = format_timestamp_input(self.clip_end_sec);

        if is_twitter_gif_info(&info) {
            self.clip_format = MediaFormat::ImageGif;
        }
        self.clip_analyzed_info = Some(info);
        self.recalculate_clip_size();
    }

    fn recalculate_main_size(&mut self) {
        self.main_locked_format_id = None;

        if self.is_playlist_mode {
            let checked: Vec<&PlaylistRowState> =
                self.playlist_rows.iter().filter(|r| r.checked).collect();
            if checked.is_empty() {
                self.main_size_label = "Peso estimado: 0 seleccionados".into();
                return;
            }
            let v_rows: Vec<&&PlaylistRowState> = checked
                .iter()
                .filter(|r| r.format == MediaFormat::VideoMp4)
                .collect();
            let a_rows: Vec<&&PlaylistRowState> = checked
                .iter()
                .filter(|r| r.format == MediaFormat::AudioMp3)
                .collect();

            let mut total_audio_bytes: u64 = 0;
            for r in &a_rows {
                if let Some(d) = r.duration {
                    if d > 0.0 {
                        let kbps = r.audio_quality.kbps() as f64;
                        total_audio_bytes += ((kbps * 1000.0 / 8.0) * d) as u64;
                    }
                }
            }
            if !v_rows.is_empty() && !a_rows.is_empty() {
                self.main_size_label = if total_audio_bytes > 0 {
                    format!(
                        "Seleccionados: {} MP4 + {} MP3 ({} audio)",
                        v_rows.len(),
                        a_rows.len(),
                        format_bytes(total_audio_bytes, true)
                    )
                } else {
                    format!(
                        "Seleccionados: {} MP4 + {} MP3",
                        v_rows.len(),
                        a_rows.len()
                    )
                };
            } else if !a_rows.is_empty() {
                self.main_size_label = if total_audio_bytes > 0 {
                    format!(
                        "Peso total ({} MP3): {}",
                        a_rows.len(),
                        format_bytes(total_audio_bytes, true)
                    )
                } else {
                    format!("Seleccionados: {} pistas MP3", a_rows.len())
                };
            } else {
                let first_q = v_rows[0].video_quality;
                let all_same = v_rows.iter().all(|r| r.video_quality == first_q);
                let q_tag = if all_same {
                    first_q.short_label()
                } else {
                    "Calidades individuales"
                };
                self.main_size_label =
                    format!("Seleccionados: {} videos MP4 ({q_tag})", v_rows.len());
            }
            return;
        }

        if self.main_format == MediaFormat::ImageGif {
            self.main_size_label = "Peso estimado: No aplica en Imagen / GIF".into();
            return;
        }

        let Some(ref info) = self.main_analyzed_info else {
            self.main_size_label = "Peso estimado: Pega un enlace".into();
            return;
        };

        if self.main_format == MediaFormat::AudioMp3 {
            let kbps = self.main_audio_quality.kbps();
            if let Some(dur) = info.duration {
                if dur > 0.0 {
                    let bytes = (((kbps as f64) * 1000.0 / 8.0) * dur) as u64;
                    self.main_size_label =
                        format!("Peso final ({kbps} kbps): {}", format_bytes(bytes, true));
                    return;
                }
            }
            let best_a = info
                .formats
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter(|f| f.acodec.as_deref() != Some("none"))
                .map(|f| stream_bytes(f, None))
                .max()
                .unwrap_or(0);
            self.main_size_label =
                format!("Peso estimado ({kbps} kbps): {}", format_bytes(best_a, false));
            return;
        }

        let url_hint = self.main_analyzed_url.as_deref().unwrap_or("");
        if let Some(sel) = select_optimal_video_format(
            info,
            self.main_video_quality,
            url_hint,
            self.ffmpeg_path.is_some(),
        ) {
            self.main_locked_format_id = Some(sel.locked_id);
            let res_tag = resolution_label(&sel.best_v);
            let prefix = if sel.is_exact {
                "Peso exacto"
            } else {
                "Peso estimado"
            };
            self.main_size_label = format!(
                "{prefix} ({res_tag}): {}",
                format_bytes(sel.total_bytes, sel.is_exact)
            );
        } else {
            let fb = info.filesize.or(info.filesize_approx).unwrap_or(0);
            self.main_size_label = format!("Peso estimado: {}", format_bytes(fb, false));
        }
    }

    fn recalculate_clip_size(&mut self) {
        self.clip_locked_format_id = None;
        let clip_dur = (self.clip_end_sec - self.clip_start_sec).max(0.5);

        match self.clip_format {
            MediaFormat::AudioMp3 => {
                let kbps = self.clip_audio_quality.kbps();
                let bytes = (((kbps as f64) * 1000.0 / 8.0) * clip_dur) as u64;
                self.clip_size_label = format!(
                    "Peso del clip MP3 ({kbps} kbps): {}",
                    format_bytes(bytes, true)
                );
            }
            MediaFormat::ImageGif => {
                let (max_h, fps) = self.clip_gif_quality.params();
                let res_lbl = max_h
                    .map(|h| format!("{h}p"))
                    .unwrap_or_else(|| "Original".into());
                let dur_str = format_duration(Some(clip_dur));
                self.clip_size_label =
                    format!("Clip GIF ({res_lbl} • {fps} FPS) | Duración: {dur_str}");
            }
            MediaFormat::VideoMp4 => {
                let Some(ref info) = self.clip_analyzed_info else {
                    self.clip_size_label = "Peso del fragmento: Pega un enlace".into();
                    return;
                };
                let full_dur = info.duration.unwrap_or(0.0);
                let ratio = if full_dur > 0.0 {
                    (clip_dur / full_dur).clamp(0.01, 1.0)
                } else {
                    1.0
                };
                let url_hint = self.clip_analyzed_url.as_deref().unwrap_or("");
                if let Some(sel) = select_optimal_video_format(
                    info,
                    self.clip_video_quality,
                    url_hint,
                    self.ffmpeg_path.is_some(),
                ) {
                    self.clip_locked_format_id = Some(sel.locked_id);
                    let clip_bytes = ((sel.total_bytes as f64) * ratio) as u64;
                    let res_tag = resolution_label(&sel.best_v);
                    self.clip_size_label = format!(
                        "Peso estimado del clip ({res_tag}): {}",
                        format_bytes(clip_bytes, false)
                    );
                } else {
                    let fb = info.filesize.or(info.filesize_approx).unwrap_or(0);
                    let clip_bytes = ((fb as f64) * ratio) as u64;
                    self.clip_size_label = format!(
                        "Peso estimado del clip: {}",
                        format_bytes(clip_bytes, false)
                    );
                }
            }
        }
    }

    fn trigger_main_analysis(&mut self, ctx: &egui::Context, force: bool) {
        self.main_debounce_deadline = None;
        let url = self.main_url.trim().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return;
        }
        if !force
            && self.main_analyzed_url.as_deref() == Some(&url)
            && self.main_analyzed_info.is_some()
        {
            self.recalculate_main_size();
            return;
        }
        if !force {
            if let Some(cached) = self.url_cache.get(&url).cloned() {
                self.main_analyze_gen += 1;
                let gen_id = self.main_analyze_gen;
                self.apply_main_analyzed_info(ctx, url, cached, gen_id);
                return;
            }
        }

        self.main_analyze_gen += 1;
        let gen_id = self.main_analyze_gen;
        self.main_analyzed_url = Some(url.clone());
        self.main_analyzed_info = None;
        self.main_locked_format_id = None;
        self.main_size_label = "Peso estimado: Calculando...".into();
        self.main_media_info_text = "Analizando flujos del enlace...".into();

        let ytdlp = self.ytdlp_path.clone();
        let qjs = self.qjs_path.clone();
        let tx = self.tx.clone();
        let ctx_c = ctx.clone();

        std::thread::spawn(move || {
            match analyze_url(&url, &ytdlp, qjs.as_deref(), true, None) {
                Ok(info) => {
                    let _ = tx.send(UiMessage::MainAnalyzed {
                        generation: gen_id,
                        url,
                        info,
                    });
                }
                Err(error) => {
                    let _ = tx.send(UiMessage::MainAnalyzeFailed {
                        generation: gen_id,
                        error,
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    fn trigger_clip_analysis(&mut self, ctx: &egui::Context, force: bool) {
        self.clip_debounce_deadline = None;
        let url = self.clip_url.trim().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            self.clip_size_label = "Peso del fragmento: Pega un enlace válido".into();
            return;
        }
        let is_same_url = self.clip_analyzed_url.as_deref() == Some(&url);
        if !is_same_url {
            self.clip_total_duration = 0.0;
            self.clip_start_sec = 0.0;
            self.clip_end_sec = 15.0;
            self.clip_start_text = "00:00".into();
            self.clip_end_text = "00:15".into();
        }
        if !force && is_same_url && self.clip_analyzed_info.is_some() {
            self.recalculate_clip_size();
            return;
        }
        if !force {
            if let Some(cached) = self.url_cache.get(&url).cloned() {
                if !cached.is_playlist {
                    self.clip_analyze_gen += 1;
                    let gen_id = self.clip_analyze_gen;
                    self.apply_clip_analyzed_info(ctx, url, cached, gen_id);
                    return;
                }
            }
        }

        self.clip_analyze_gen += 1;
        let gen_id = self.clip_analyze_gen;
        self.clip_analyzed_url = Some(url.clone());
        self.clip_analyzed_info = None;
        self.clip_locked_format_id = None;
        self.clip_loading = true;
        self.clip_size_label = "Analizando duración y calidades del video...".into();
        self.clip_media_info_text = "Extrayendo información del video...".into();

        let ytdlp = self.ytdlp_path.clone();
        let qjs = self.qjs_path.clone();
        let tx = self.tx.clone();
        let ctx_c = ctx.clone();

        std::thread::spawn(move || {
            match analyze_url(&url, &ytdlp, qjs.as_deref(), false, None) {
                Ok(info) => {
                    let _ = tx.send(UiMessage::ClipAnalyzed {
                        generation: gen_id,
                        url,
                        info,
                    });
                }
                Err(error) => {
                    let _ = tx.send(UiMessage::ClipAnalyzeFailed {
                        generation: gen_id,
                        error,
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    fn clear_main_url(&mut self) {
        self.main_debounce_deadline = None;
        self.main_url.clear();
        self.main_last_edited_url.clear();
        self.main_analyzed_url = None;
        self.main_analyzed_info = None;
        self.main_locked_format_id = None;
        self.is_playlist_mode = false;
        self.playlist_rows.clear();
        self.main_thumb_tex = None;
        self.main_media_info_text =
            "Pega un enlace para calcular calidades y peso automáticamente".into();
        self.main_size_label = "Peso estimado: Pega un enlace".into();
    }

    fn clear_clip_url(&mut self) {
        self.clip_debounce_deadline = None;
        self.clip_url.clear();
        self.clip_last_edited_url.clear();
        self.clip_analyzed_url = None;
        self.clip_analyzed_info = None;
        self.clip_locked_format_id = None;
        self.clip_total_duration = 0.0;
        self.clip_start_sec = 0.0;
        self.clip_end_sec = 15.0;
        self.clip_start_text = "00:00".into();
        self.clip_end_text = "00:15".into();
        self.clip_thumb_tex = None;
        self.clip_loading = false;
        self.clip_media_info_text =
            "Pega un enlace para cargar la duración y elegir el fragmento".into();
        self.clip_size_label = "Peso del fragmento: Pega un enlace".into();
    }

    fn start_main_download(&mut self, ctx: &egui::Context) {
        if self.is_busy {
            return;
        }
        if self.is_playlist_mode {
            let checked: Vec<PlaylistRowState> = self
                .playlist_rows
                .iter()
                .filter(|r| r.checked && !r.url.is_empty())
                .cloned()
                .collect();
            if checked.is_empty() {
                self.status_text = "Marca al menos un elemento de la lista".into();
                self.percent_text = "0%".into();
                self.progress = 0.0;
                self.push_log("[AVISO] No hay ningún elemento seleccionado en la lista.");
                return;
            }

            let total = checked.len();
            self.cancel_token.reset();
            self.is_busy = true;
            self.busy_action_text = format!("DESCARGANDO LISTA (0/{total})...");
            self.status_text = format!("Iniciando descarga de {total} elementos...");
            self.progress = 0.0;
            self.percent_text = "0%".into();
            self.push_log("-".repeat(65));
            self.push_log(format!(
                "Iniciando descarga por lotes: {total} elementos seleccionados."
            ));

            let download_dir = self.download_dir.clone();
            let ytdlp = self.ytdlp_path.clone();
            let ffmpeg = self.ffmpeg_path.clone();
            let qjs = self.qjs_path.clone();
            let tx = self.tx.clone();
            let cancel = self.cancel_token.clone();
            let ctx_c = ctx.clone();

            std::thread::spawn(move || {
                let mut ok_count = 0usize;
                let mut fail_count = 0usize;

                for (idx, row) in checked.iter().enumerate() {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let step = idx + 1;
                    let q_lbl = match row.format {
                        MediaFormat::AudioMp3 => row.audio_quality.short_label(),
                        _ => row.video_quality.short_label(),
                    };
                    let short_t: String = row.title.chars().take(38).collect();
                    let _ = tx.send(UiMessage::Busy {
                        busy: true,
                        action_text: Some(format!("DESCARGANDO LISTA ({step}/{total})...")),
                    });
                    let _ = tx.send(UiMessage::Log(format!(
                        "[{step}/{total}] Descargando ({} | {q_lbl}): {}",
                        row.format.label(),
                        row.title
                    )));
                    let frac = (idx as f32) / (total as f32);
                    let _ = tx.send(UiMessage::Status {
                        text: format!("[{step}/{total}] ({} {q_lbl}) {short_t}...", row.format.label()),
                        progress: Some(frac),
                        percent: Some(format!("{}%", (frac * 100.0) as u32)),
                    });
                    ctx_c.request_repaint();

                    match execute_single_download(
                        &row.url,
                        None,
                        row.format,
                        row.video_quality,
                        row.audio_quality,
                        None,
                        &download_dir,
                        &ytdlp,
                        ffmpeg.as_deref(),
                        qjs.as_deref(),
                        &tx,
                        &cancel,
                    ) {
                        Ok(_) => {
                            ok_count += 1;
                            let _ = tx.send(UiMessage::Log(format!(
                                "[OK {step}/{total}] Completado ({}): {}",
                                row.format.label(),
                                row.title
                            )));
                        }
                        Err(e) => {
                            if cancel.is_cancelled() {
                                break;
                            }
                            fail_count += 1;
                            let _ = tx.send(UiMessage::Log(format!(
                                "[ERROR {step}/{total}] Falló '{}': {}",
                                row.title,
                                clean_ansi(&e)
                            )));
                        }
                    }
                }

                let (final_status, final_percent, progress) = if cancel.is_cancelled() {
                    let _ = tx.send(UiMessage::Log("[CANCELADO] Descarga de lista detenida por el usuario.".into()));
                    ("Descarga cancelada".into(), "Cancelado".into(), 0.0)
                } else if fail_count == 0 {
                    (
                        format!("¡Lista completada! ({ok_count}/{total} descargados con éxito)"),
                        "100%".into(),
                        1.0,
                    )
                } else {
                    (
                        format!("Lista finalizada: {ok_count} exitosos, {fail_count} con error"),
                        "100%".into(),
                        1.0,
                    )
                };

                let _ = tx.send(UiMessage::DownloadFinished {
                    final_status,
                    final_percent,
                    progress,
                });
                ctx_c.request_repaint();
            });
            return;
        }

        let url = self.main_url.trim().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            self.status_text = "Introduce una URL válida (http:// o https://)".into();
            self.progress = 0.0;
            self.percent_text = "0%".into();
            self.push_log("[ERROR] La URL ingresada está vacía o no tiene un formato válido.");
            return;
        }

        self.cancel_token.reset();
        self.is_busy = true;
        self.busy_action_text = "PROCESANDO DESCARGA...".into();
        self.status_text = "Iniciando descarga...".into();
        self.progress = 0.0;
        self.percent_text = "0%".into();
        self.push_log("-".repeat(65));
        self.push_log(format!("Iniciando descarga: {url}"));

        let sel_fmt = self.main_format;
        let vid_q = self.main_video_quality;
        let aud_q = self.main_audio_quality;
        let q_lbl = match sel_fmt {
            MediaFormat::AudioMp3 => aud_q.label(),
            MediaFormat::VideoMp4 => vid_q.label(),
            MediaFormat::ImageGif => "No aplica (Original)",
        };
        self.push_log(format!("Formato: {} | Calidad: {q_lbl}", sel_fmt.label()));

        let mut cached_info = if self.main_analyzed_url.as_deref() == Some(&url) {
            self.main_analyzed_info.clone()
        } else {
            None
        };
        let mut locked_id = if cached_info.is_some() {
            self.main_locked_format_id.clone()
        } else {
            None
        };
        if let Some(ref lid) = locked_id {
            if sel_fmt == MediaFormat::VideoMp4 {
                self.push_log(format!("Flujo exacto seleccionado (ID): {lid}"));
            }
        }

        let download_dir = self.download_dir.clone();
        let ytdlp = self.ytdlp_path.clone();
        let ffmpeg = self.ffmpeg_path.clone();
        let qjs = self.qjs_path.clone();
        let gen_id = self.main_analyze_gen;
        let tx = self.tx.clone();
        let cancel = self.cancel_token.clone();
        let ctx_c = ctx.clone();

        std::thread::spawn(move || {
            // Sondeo automático sin tocar widgets UI si el usuario no esperó al pre-análisis
            if cached_info.is_none() && sel_fmt == MediaFormat::VideoMp4 && locked_id.is_none() {
                if let Ok(probed) = analyze_url(&url, &ytdlp, qjs.as_deref(), false, Some(&cancel))
                {
                    if let Some(sel) =
                        select_optimal_video_format(&probed, vid_q, &url, ffmpeg.is_some())
                    {
                        let _ = tx.send(UiMessage::Log(format!(
                            "Flujo óptimo verificado automáticamente (ID): {}",
                            sel.locked_id
                        )));
                        locked_id = Some(sel.locked_id);
                    }
                    let _ = tx.send(UiMessage::MainAnalyzed {
                        generation: gen_id,
                        url: url.clone(),
                        info: probed.clone(),
                    });
                    cached_info = Some(probed);
                }
            }

            if cancel.is_cancelled() {
                let _ = tx.send(UiMessage::Log(
                    "[CANCELADO] Descarga detenida por el usuario.".into(),
                ));
                let _ = tx.send(UiMessage::DownloadFinished {
                    final_status: "Descarga cancelada".into(),
                    final_percent: "Cancelado".into(),
                    progress: 0.0,
                });
                ctx_c.request_repaint();
                return;
            }

            let res = execute_single_download(
                &url,
                cached_info.as_ref(),
                sel_fmt,
                vid_q,
                aud_q,
                locked_id.as_deref(),
                &download_dir,
                &ytdlp,
                ffmpeg.as_deref(),
                qjs.as_deref(),
                &tx,
                &cancel,
            );

            match res {
                Ok(_) => {
                    let _ = tx.send(UiMessage::Log(
                        "PROCESO FINALIZADO: Archivo guardado en la carpeta de descargas.".into(),
                    ));
                    let _ = tx.send(UiMessage::DownloadFinished {
                        final_status: "¡Descarga completada con éxito!".into(),
                        final_percent: "100%".into(),
                        progress: 1.0,
                    });
                }
                Err(err) => {
                    if cancel.is_cancelled() {
                        let _ = tx.send(UiMessage::Log(
                            "[CANCELADO] Descarga detenida por el usuario.".into(),
                        ));
                        let _ = tx.send(UiMessage::DownloadFinished {
                            final_status: "Descarga cancelada".into(),
                            final_percent: "Cancelado".into(),
                            progress: 0.0,
                        });
                    } else {
                        let _ = tx.send(UiMessage::Log(format!(
                            "[ERROR DE DESCARGA] {}",
                            clean_ansi(&err)
                        )));
                        let _ = tx.send(UiMessage::DownloadFinished {
                            final_status: "Error durante la descarga (revisa el registro)".into(),
                            final_percent: "Error".into(),
                            progress: 0.0,
                        });
                    }
                }
            }
            ctx_c.request_repaint();
        });
    }

    fn start_clip_download(&mut self, ctx: &egui::Context) {
        if self.is_busy {
            return;
        }
        let url = self.clip_url.trim().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            self.status_text = "Introduce una URL válida en la pestaña de recorte".into();
            self.progress = 0.0;
            self.percent_text = "0%".into();
            self.push_log("[ERROR] La URL del módulo de recorte está vacía o no es válida.");
            return;
        }
        self.commit_clip_time_entries();
        if self.clip_end_sec <= self.clip_start_sec {
            self.status_text = "El tiempo de Fin debe ser mayor al tiempo de Inicio".into();
            self.progress = 0.0;
            self.percent_text = "0%".into();
            self.push_log("[ERROR] Rango de recorte inválido: Fin debe ser mayor que Inicio.");
            return;
        }

        let start_sec = self.clip_start_sec;
        let end_sec = self.clip_end_sec;
        let sel_fmt = self.clip_format;
        let vid_q = self.clip_video_quality;
        let aud_q = self.clip_audio_quality;
        let gif_q = self.clip_gif_quality;
        let range_tag = format!(
            "{} -> {}",
            format_timestamp_input(start_sec),
            format_timestamp_input(end_sec)
        );

        self.cancel_token.reset();
        self.is_busy = true;
        self.busy_action_text = "RECORTANDO Y DESCARGANDO...".into();
        self.status_text = format!("Recortando fragmento ({range_tag})...");
        self.progress = 0.05;
        self.percent_text = "5%".into();
        self.push_log("-".repeat(65));
        self.push_log(format!("Iniciando recorte de fragmento: {url}"));
        let q_lbl = match sel_fmt {
            MediaFormat::VideoMp4 => vid_q.label(),
            MediaFormat::AudioMp3 => aud_q.label(),
            MediaFormat::ImageGif => gif_q.label(),
        };
        self.push_log(format!(
            "Segmento: {range_tag} | Formato: {} | Calidad: {q_lbl}",
            sel_fmt.label()
        ));

        let cached_info = if self.clip_analyzed_url.as_deref() == Some(&url) {
            self.clip_analyzed_info.clone()
        } else {
            None
        };
        let locked_id = if cached_info.is_some() {
            self.clip_locked_format_id.clone()
        } else {
            None
        };

        let download_dir = self.download_dir.clone();
        let ytdlp = self.ytdlp_path.clone();
        let ffmpeg = self.ffmpeg_path.clone();
        let qjs = self.qjs_path.clone();
        let tx = self.tx.clone();
        let cancel = self.cancel_token.clone();
        let ctx_c = ctx.clone();

        std::thread::spawn(move || {
            let res = execute_clip_download(
                &url,
                cached_info.as_ref(),
                start_sec,
                end_sec,
                sel_fmt,
                vid_q,
                aud_q,
                gif_q,
                locked_id.as_deref(),
                &download_dir,
                &ytdlp,
                ffmpeg.as_deref(),
                qjs.as_deref(),
                &tx,
                &cancel,
            );
            match res {
                Ok(fp_opt) => {
                    if let Some(fp) = fp_opt {
                        let name = fp.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        let _ = tx.send(UiMessage::Log(format!(
                            "[ARCHIVO] Fragmento guardado: {name}"
                        )));
                    }
                    let _ = tx.send(UiMessage::Log(
                        "PROCESO FINALIZADO: El fragmento recortado se guardó en la carpeta de descargas.".into(),
                    ));
                    let _ = tx.send(UiMessage::DownloadFinished {
                        final_status: "¡Fragmento recortado y descargado con éxito!".into(),
                        final_percent: "100%".into(),
                        progress: 1.0,
                    });
                }
                Err(err) => {
                    if cancel.is_cancelled() {
                        let _ = tx.send(UiMessage::Log(
                            "[CANCELADO] Recorte detenido por el usuario.".into(),
                        ));
                        let _ = tx.send(UiMessage::DownloadFinished {
                            final_status: "Recorte cancelado".into(),
                            final_percent: "Cancelado".into(),
                            progress: 0.0,
                        });
                    } else {
                        let _ = tx.send(UiMessage::Log(format!(
                            "[ERROR DE RECORTE] {}",
                            clean_ansi(&err)
                        )));
                        let _ = tx.send(UiMessage::DownloadFinished {
                            final_status: "Error al recortar el fragmento (revisa el registro)".into(),
                            final_percent: "Error".into(),
                            progress: 0.0,
                        });
                    }
                }
            }
            ctx_c.request_repaint();
        });
    }

    fn commit_clip_time_entries(&mut self) {
        if let Some(s) = parse_timestamp_input(&self.clip_start_text) {
            let capped = if self.clip_total_duration > 0.0 {
                s.clamp(0.0, (self.clip_total_duration - 0.5).max(0.0))
            } else {
                s.max(0.0)
            };
            self.clip_start_sec = capped;
        }
        if let Some(e) = parse_timestamp_input(&self.clip_end_text) {
            let capped = if self.clip_total_duration > 0.0 {
                e.clamp(0.5, self.clip_total_duration)
            } else {
                e.max(0.5)
            };
            self.clip_end_sec = capped;
        }
        let max_limit = if self.clip_total_duration > 0.0 {
            self.clip_total_duration
        } else {
            (self.clip_start_sec + 60.0)
                .max(self.clip_end_sec + 60.0)
                .max(300.0)
        };
        if self.clip_end_sec <= self.clip_start_sec {
            self.clip_end_sec = max_limit.min(self.clip_start_sec + 15.0);
            if self.clip_end_sec <= self.clip_start_sec {
                self.clip_start_sec = (self.clip_end_sec - 1.0).max(0.0);
            }
        }
        self.clip_start_text = format_timestamp_input(self.clip_start_sec);
        self.clip_end_text = format_timestamp_input(self.clip_end_sec);
        self.recalculate_clip_size();
    }

    fn choose_download_folder(&mut self) {
        if let Some(picked) = rfd::FileDialog::new()
            .set_directory(&self.download_dir)
            .set_title("Seleccionar carpeta de descarga")
            .pick_folder()
        {
            self.download_dir = picked.clone();
            self.push_log(format!(
                "Carpeta de destino actualizada: {}",
                self.download_dir.display()
            ));
            self.config.download_dir = Some(self.download_dir.to_string_lossy().to_string());
            save_config(&self.config);
        }
    }

    fn open_download_folder(&self) {
        let _ = std::fs::create_dir_all(&self.download_dir);
        #[cfg(target_os = "windows")]
        {
            let mut cmd = Command::new("explorer");
            cmd.arg(&self.download_dir);
            apply_no_window(&mut cmd);
            let _ = cmd.spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = Command::new("xdg-open").arg(&self.download_dir).spawn();
        }
    }

    fn card_frame(&self) -> Frame {
        Frame::new()
            .fill(self.palette.bg_card)
            .stroke(Stroke::new(1.0_f32, self.palette.border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 12))
    }

    fn subcard_frame(&self) -> Frame {
        Frame::new()
            .fill(self.palette.bg_subcard)
            .stroke(Stroke::new(1.0_f32, self.palette.border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 8))
    }

    fn styled_button(&self, ui: &mut Ui, label: &str, fill: Color32, size: Vec2) -> bool {
        let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(fill)
            .corner_radius(CornerRadius::same(5))
            .min_size(size);
        ui.add(btn).clicked()
    }

    fn render_preview_card(
        &self,
        ui: &mut Ui,
        tex: Option<&TextureHandle>,
        title: &str,
        meta: &str,
    ) {
        self.subcard_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(t) = tex {
                    ui.image((t.id(), egui::vec2(88.0, 50.0)));
                }
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(title)
                            .size(13.0)
                            .strong()
                            .color(self.palette.text_primary),
                    );
                    ui.label(
                        RichText::new(meta)
                            .size(11.5)
                            .color(self.palette.text_secondary),
                    );
                });
            });
        });
    }

    fn render_folder_row(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Guardar en: {}", self.download_dir.display()))
                    .size(11.5)
                    .color(self.palette.text_secondary),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.styled_button(
                    ui,
                    "Abrir Destino",
                    self.palette.btn_secondary,
                    egui::vec2(105.0, 30.0),
                ) {
                    self.open_download_folder();
                }
                ui.add_enabled_ui(!self.is_busy, |ui| {
                    if self.styled_button(
                        ui,
                        "Cambiar Carpeta",
                        self.palette.btn_secondary,
                        egui::vec2(120.0, 30.0),
                    ) {
                        self.choose_download_folder();
                    }
                });
            });
        });
    }
}

impl eframe::App for UniversalDownloaderApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cancel_token.cancel_blocking();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages(ctx);

        // Procesar timers de debounce (300 ms) para análisis automático de URLs
        let now = Instant::now();
        if self.engines_ready {
            if let Some(deadline) = self.main_debounce_deadline {
                if now >= deadline {
                    self.trigger_main_analysis(ctx, false);
                } else {
                    ctx.request_repaint_after(deadline - now);
                }
            }
            if let Some(deadline) = self.clip_debounce_deadline {
                if now >= deadline {
                    self.trigger_clip_analysis(ctx, false);
                } else {
                    ctx.request_repaint_after(deadline - now);
                }
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.is_busy || self.clip_loading {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        let panel_frame = Frame::new()
            .fill(self.palette.bg_root)
            .inner_margin(Margin::symmetric(16, 12));

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // =========================================================================
                // CABECERA SUPERIOR
                // =========================================================================
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Universal Downloader")
                                .size(22.0)
                                .strong()
                                .color(self.palette.text_primary),
                        );
                        ui.label(
                            RichText::new(
                                "YouTube • TikTok • Instagram • Facebook • Twitter (X)",
                            )
                            .size(12.0)
                            .color(self.palette.text_secondary),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let prev_theme = self.theme_mode;
                        egui::ComboBox::from_id_salt("theme_selector")
                            .width(95.0)
                            .selected_text(self.theme_mode.label())
                            .show_ui(ui, |ui| {
                                for m in ThemeMode::ALL {
                                    ui.selectable_value(&mut self.theme_mode, m, m.label());
                                }
                            });
                        if self.theme_mode != prev_theme {
                            self.palette = apply_theme(ctx, self.theme_mode);
                            self.config.theme = Some(self.theme_mode.label().to_string());
                            save_config(&self.config);
                        }

                        ui.add_enabled_ui(self.engines_ready && !self.is_busy, |ui| {
                            let (btn_lbl, btn_fill) = match &self.update_available_text {
                                Some(t) => (t.as_str(), self.palette.btn_success),
                                None => ("Actualizar API", self.palette.accent),
                            };
                            if self.styled_button(ui, btn_lbl, btn_fill, egui::vec2(118.0, 32.0)) {
                                self.cancel_token.reset();
                                self.is_busy = true;
                                self.busy_action_text = "ACTUALIZANDO MOTOR API...".into();
                                self.push_log("-".repeat(65));
                                self.push_log(
                                    "Iniciando actualización del motor de extracción (yt-dlp)...",
                                );
                                self.status_text =
                                    "Descargando última versión del motor de descargas...".into();
                                self.progress = 0.05;
                                self.percent_text = "5%".into();

                                let ytdlp = self.ytdlp_path.clone();
                                let mut cfg_bg = self.config.clone();
                                let cancel = self.cancel_token.clone();
                                let tx = self.tx.clone();
                                let ctx_c = ctx.clone();
                                std::thread::spawn(move || {
                                    let res = download_ytdlp_update(
                                        &ytdlp,
                                        |frac| {
                                            let _ = tx.send(UiMessage::Status {
                                                text: "Descargando nuevo motor API desde GitHub..."
                                                    .into(),
                                                progress: Some(frac),
                                                percent: Some(format!("{}%", (frac * 100.0) as u32)),
                                            });
                                            ctx_c.request_repaint();
                                        },
                                        || cancel.is_cancelled(),
                                    );
                                    match res {
                                        Ok(()) => {
                                            let ver = detect_local_ytdlp_version(&ytdlp, &mut cfg_bg);
                                            let _ = tx.send(UiMessage::UpdateFinished {
                                                engine_version: ver,
                                                config: cfg_bg,
                                            });
                                            let _ = tx.send(UiMessage::Log(
                                                "[ÉXITO] Motor yt-dlp.exe actualizado correctamente.".into(),
                                            ));
                                            let _ = tx.send(UiMessage::DownloadFinished {
                                                final_status: "¡API actualizada exitosamente!".into(),
                                                final_percent: "100%".into(),
                                                progress: 1.0,
                                            });
                                        }
                                        Err(e) => {
                                            if cancel.is_cancelled() {
                                                let _ = tx.send(UiMessage::Log(
                                                    "[CANCELADO] Actualización detenida por el usuario.".into(),
                                                ));
                                                let _ = tx.send(UiMessage::DownloadFinished {
                                                    final_status: "Actualización cancelada".into(),
                                                    final_percent: "Cancelado".into(),
                                                    progress: 0.0,
                                                });
                                            } else {
                                                let _ = tx.send(UiMessage::Log(format!(
                                                    "[ERROR DE ACTUALIZACIÓN] {e}"
                                                )));
                                                let _ = tx.send(UiMessage::DownloadFinished {
                                                    final_status: "Error al actualizar la API (revisa el registro)".into(),
                                                    final_percent: "Error".into(),
                                                    progress: 0.0,
                                                });
                                            }
                                        }
                                    }
                                    ctx_c.request_repaint();
                                });
                            }

                            if self.styled_button(
                                ui,
                                "Consultar Actualización",
                                self.palette.btn_secondary,
                                egui::vec2(148.0, 32.0),
                            ) {
                                self.push_log("-".repeat(65));
                                self.push_log(
                                    "Consultando si existen nuevas actualizaciones del motor de descarga...",
                                );
                                self.status_text =
                                    "Consultando última versión de la API en servidores oficiales..."
                                        .into();
                                self.progress = 0.3;
                                self.percent_text = "...".into();

                                let ytdlp = self.ytdlp_path.clone();
                                let mut cfg_bg = self.config.clone();
                                let tx = self.tx.clone();
                                let ctx_c = ctx.clone();
                                std::thread::spawn(move || {
                                    let local_ver = detect_local_ytdlp_version(&ytdlp, &mut cfg_bg);
                                    if let Some(latest) = fetch_latest_remote_version() {
                                        if is_newer_version(&latest, &local_ver) {
                                            let _ = tx.send(UiMessage::UpdateAvailable {
                                                local_ver,
                                                latest_ver: latest,
                                            });
                                        } else {
                                            let msg =
                                                format!("API de descargas al día (v{local_ver})");
                                            let _ = tx.send(UiMessage::Log(format!(
                                                "[OK] {msg}. No se requieren actualizaciones."
                                            )));
                                            let _ = tx.send(UiMessage::Status {
                                                text: msg,
                                                progress: Some(1.0),
                                                percent: Some("Al día".into()),
                                            });
                                        }
                                    } else {
                                        let _ = tx.send(UiMessage::Log(
                                            "[AVISO] No fue posible consultar la versión remota (verifica tu conexión).".into(),
                                        ));
                                        let _ = tx.send(UiMessage::Status {
                                            text: "No se pudo conectar al servidor de actualizaciones".into(),
                                            progress: Some(0.0),
                                            percent: Some("0%".into()),
                                        });
                                    }
                                    ctx_c.request_repaint();
                                });
                            }
                        });
                    });
                });

                ui.add_space(6.0);

                // =========================================================================
                // SELECTOR DE PESTAÑAS
                // =========================================================================
                self.subcard_frame().show(ui, |ui| {
                    ui.columns(2, |cols| {
                        let tab_main_active = self.active_tab == ActiveTab::Main;
                        let fill_main = if tab_main_active {
                            self.palette.accent
                        } else {
                            Color32::TRANSPARENT
                        };
                        let txt_main = if tab_main_active {
                            Color32::WHITE
                        } else {
                            self.palette.text_primary
                        };
                        if cols[0]
                            .add(
                                egui::Button::new(
                                    RichText::new("Descarga General / Listas")
                                        .size(13.0)
                                        .strong()
                                        .color(txt_main),
                                )
                                .fill(fill_main)
                                .min_size(egui::vec2(cols[0].available_width(), 32.0)),
                            )
                            .clicked()
                        {
                            self.active_tab = ActiveTab::Main;
                        }

                        let tab_clip_active = self.active_tab == ActiveTab::Clip;
                        let fill_clip = if tab_clip_active {
                            self.palette.accent
                        } else {
                            Color32::TRANSPARENT
                        };
                        let txt_clip = if tab_clip_active {
                            Color32::WHITE
                        } else {
                            self.palette.text_primary
                        };
                        if cols[1]
                            .add(
                                egui::Button::new(
                                    RichText::new("Recortar Fragmento (Clip)")
                                        .size(13.0)
                                        .strong()
                                        .color(txt_clip),
                                )
                                .fill(fill_clip)
                                .min_size(egui::vec2(cols[1].available_width(), 32.0)),
                            )
                            .clicked()
                        {
                            self.active_tab = ActiveTab::Clip;
                        }
                    });
                });

                ui.add_space(4.0);

                // =========================================================================
                // CONTENIDO DE LA PESTAÑA ACTIVA
                // =========================================================================
                match self.active_tab {
                    ActiveTab::Main => self.render_main_tab(ctx, ui),
                    ActiveTab::Clip => self.render_clip_tab(ctx, ui),
                }

                ui.add_space(6.0);

                // =========================================================================
                // ESTADO + PROGRESO + CONSOLA DE ACTIVIDAD
                // =========================================================================
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Estado: {}", self.status_text))
                            .size(12.5)
                            .strong()
                            .color(self.palette.text_primary),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(&self.percent_text)
                                .size(12.5)
                                .strong()
                                .color(self.palette.text_accent),
                        );
                    });
                });

                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .desired_height(12.0)
                        .fill(self.palette.accent),
                );

                ui.add_space(4.0);

                self.card_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("Registro de actividad en tiempo real:")
                            .size(12.0)
                            .strong()
                            .color(self.palette.text_primary),
                    );
                    ui.add_space(4.0);
                    Frame::new()
                        .fill(self.palette.bg_console)
                        .stroke(Stroke::new(1.0_f32, self.palette.border))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("log_scroll")
                                .stick_to_bottom(true)
                                .auto_shrink([false, false])
                                .min_scrolled_height(130.0)
                                .show(ui, |ui| {
                                    for line in &self.logs {
                                        ui.label(
                                            RichText::new(line)
                                                .monospace()
                                                .size(11.5)
                                                .color(self.palette.text_primary),
                                        );
                                    }
                                });
                        });
                });
            });
    }
}

impl UniversalDownloaderApp {
    fn render_main_tab(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        self.card_frame().show(ui, |ui| {
            // Cabecera de enlace
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Enlace del contenido (URL):")
                        .size(13.0)
                        .strong()
                        .color(self.palette.text_primary),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(&self.main_media_info_text)
                            .size(11.5)
                            .color(self.palette.text_accent),
                    );
                });
            });

            // Input URL + botones
            ui.horizontal(|ui| {
                let btn_block_w = 290.0;
                let entry_w = (ui.available_width() - btn_block_w).max(200.0);
                let resp = ui.add_enabled(
                    !self.is_busy,
                    egui::TextEdit::singleline(&mut self.main_url)
                        .hint_text("Pega el enlace de YouTube, TikTok, Instagram, Facebook o X...")
                        .desired_width(entry_w),
                );
                if resp.changed() && self.main_url != self.main_last_edited_url {
                    self.main_last_edited_url = self.main_url.clone();
                    let trimmed = self.main_url.trim();
                    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                        self.main_debounce_deadline =
                            Some(Instant::now() + Duration::from_millis(300));
                    } else {
                        self.main_debounce_deadline = None;
                    }
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.trigger_main_analysis(ctx, false);
                }

                ui.add_enabled_ui(self.engines_ready && !self.is_busy, |ui| {
                    if self.styled_button(
                        ui,
                        "Pegar",
                        self.palette.btn_secondary,
                        egui::vec2(72.0, 32.0),
                    ) {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            if let Ok(text) = cb.get_text() {
                                let clean = text.trim().to_string();
                                if !clean.is_empty() {
                                    self.main_url = clean.clone();
                                    self.main_last_edited_url = clean;
                                    self.trigger_main_analysis(ctx, true);
                                }
                            }
                        }
                    }
                    if self.styled_button(
                        ui,
                        "Calcular Peso",
                        self.palette.accent,
                        egui::vec2(114.0, 32.0),
                    ) {
                        self.trigger_main_analysis(ctx, true);
                    }
                    if self.styled_button(
                        ui,
                        "Limpiar",
                        self.palette.btn_secondary,
                        egui::vec2(74.0, 32.0),
                    ) {
                        self.clear_main_url();
                    }
                });
            });

            // Tarjeta vista previa (video/audio individual)
            if !self.is_playlist_mode {
                if let Some(ref info) = self.main_analyzed_info {
                    let raw_title = info
                        .title
                        .as_deref()
                        .unwrap_or("Contenido multimedia")
                        .trim();
                    let is_tw_gif = is_twitter_gif_info(info);
                    let is_suno = info.suno_mp4_url.is_some();
                    let dur_str = if is_tw_gif {
                        "GIF animado".to_string()
                    } else {
                        format_duration(info.duration)
                    };
                    let type_badge = if is_tw_gif {
                        "GIF animado".to_string()
                    } else if is_suno {
                        "Audio / Canción HD".to_string()
                    } else {
                        let mut v_fmts: Vec<_> = info
                            .formats
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .filter(|f| has_video(f))
                            .collect();
                        v_fmts.sort_by_key(|f| {
                            std::cmp::Reverse(f.width.unwrap_or(0).max(f.height.unwrap_or(0)))
                        });
                        if let Some(top) = v_fmts.first() {
                            format!("Video ({})", resolution_label(top))
                        } else {
                            "Contenido multimedia".to_string()
                        }
                    };
                    let uploader = info
                        .uploader
                        .as_deref()
                        .or(info.channel.as_deref())
                        .unwrap_or("")
                        .trim();
                    let uploader_part = if uploader.is_empty() {
                        String::new()
                    } else {
                        format!(" • {}", uploader.chars().take(28).collect::<String>())
                    };
                    let title_short: String = raw_title.chars().take(68).collect();
                    let meta = format!("{type_badge} • Duración: {dur_str}{uploader_part}");
                    let tex = self.main_thumb_tex.clone();
                    self.render_preview_card(ui, tex.as_ref(), &title_short, &meta);
                }
            }

            // Checklist de lista de reproducción
            if self.is_playlist_mode && !self.playlist_rows.is_empty() {
                let checked_count = self.playlist_rows.iter().filter(|r| r.checked).count();
                let total_items = self.playlist_rows.len();
                let mut changed = false;

                self.subcard_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Lista de reproducción ({checked_count} de {total_items} seleccionados):"
                            ))
                            .size(12.5)
                            .strong()
                            .color(self.palette.text_primary),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if self.styled_button(
                                ui,
                                "Deseleccionar todo",
                                self.palette.btn_secondary,
                                egui::vec2(130.0, 26.0),
                            ) {
                                for r in &mut self.playlist_rows {
                                    r.checked = false;
                                }
                                changed = true;
                            }
                            if self.styled_button(
                                ui,
                                "Seleccionar todo",
                                self.palette.btn_secondary,
                                egui::vec2(118.0, 26.0),
                            ) {
                                for r in &mut self.playlist_rows {
                                    r.checked = true;
                                }
                                changed = true;
                            }
                        });
                    });

                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_salt("playlist_scroll")
                        .max_height(190.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_enabled_ui(!self.is_busy, |ui| {
                                for row in &mut self.playlist_rows {
                                    ui.horizontal(|ui| {
                                        if ui.checkbox(&mut row.checked, "").changed() {
                                            changed = true;
                                        }
                                        let dur_tag = format_duration(row.duration);
                                        let short_t: String = row.title.chars().take(50).collect();
                                        ui.label(
                                            RichText::new(format!(
                                                "#{:02}  {short_t}  ({dur_tag})",
                                                row.index
                                            ))
                                            .size(12.0),
                                        );
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                // Selector de calidad por fila
                                                match row.format {
                                                    MediaFormat::AudioMp3 => {
                                                        let prev = row.audio_quality;
                                                        egui::ComboBox::from_id_salt(format!(
                                                            "pl_aq_{}",
                                                            row.index
                                                        ))
                                                        .width(120.0)
                                                        .selected_text(row.audio_quality.short_label())
                                                        .show_ui(ui, |ui| {
                                                            for q in AudioQuality::ALL {
                                                                ui.selectable_value(
                                                                    &mut row.audio_quality,
                                                                    q,
                                                                    q.short_label(),
                                                                );
                                                            }
                                                        });
                                                        if row.audio_quality != prev {
                                                            changed = true;
                                                        }
                                                    }
                                                    _ => {
                                                        let prev = row.video_quality;
                                                        egui::ComboBox::from_id_salt(format!(
                                                            "pl_vq_{}",
                                                            row.index
                                                        ))
                                                        .width(120.0)
                                                        .selected_text(row.video_quality.short_label())
                                                        .show_ui(ui, |ui| {
                                                            for q in VideoQuality::ALL {
                                                                ui.selectable_value(
                                                                    &mut row.video_quality,
                                                                    q,
                                                                    q.short_label(),
                                                                );
                                                            }
                                                        });
                                                        if row.video_quality != prev {
                                                            changed = true;
                                                        }
                                                    }
                                                }

                                                // Selector de formato por fila
                                                let prev_f = row.format;
                                                egui::ComboBox::from_id_salt(format!(
                                                    "pl_fmt_{}",
                                                    row.index
                                                ))
                                                .width(105.0)
                                                .selected_text(row.format.label())
                                                .show_ui(ui, |ui| {
                                                    for f in MediaFormat::PLAYLIST_OPTIONS {
                                                        ui.selectable_value(
                                                            &mut row.format,
                                                            f,
                                                            f.label(),
                                                        );
                                                    }
                                                });
                                                if row.format != prev_f {
                                                    changed = true;
                                                }
                                            },
                                        );
                                    });
                                }
                            });
                        });
                });

                if changed {
                    self.recalculate_main_size();
                }
            }

            // Selectores globales: Formato + Calidad + Badge Peso
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!self.is_busy, |ui| {
                    ui.label(RichText::new("Formato:").size(13.0).strong());
                    let prev_fmt = self.main_format;
                    egui::ComboBox::from_id_salt("main_format_combo")
                        .width(135.0)
                        .selected_text(self.main_format.label())
                        .show_ui(ui, |ui| {
                            for f in MediaFormat::ALL {
                                ui.selectable_value(&mut self.main_format, f, f.label());
                            }
                        });
                    if self.main_format != prev_fmt {
                        if self.is_playlist_mode
                            && matches!(
                                self.main_format,
                                MediaFormat::VideoMp4 | MediaFormat::AudioMp3
                            )
                        {
                            for r in &mut self.playlist_rows {
                                r.format = self.main_format;
                            }
                        }
                        self.recalculate_main_size();
                    }

                    ui.label(RichText::new("Calidad:").size(13.0).strong());
                    match self.main_format {
                        MediaFormat::VideoMp4 => {
                            let prev_q = self.main_video_quality;
                            egui::ComboBox::from_id_salt("main_vqual_combo")
                                .width(195.0)
                                .selected_text(self.main_video_quality.label())
                                .show_ui(ui, |ui| {
                                    for q in VideoQuality::ALL {
                                        ui.selectable_value(
                                            &mut self.main_video_quality,
                                            q,
                                            q.label(),
                                        );
                                    }
                                });
                            if self.main_video_quality != prev_q {
                                if self.is_playlist_mode {
                                    for r in &mut self.playlist_rows {
                                        if r.format == MediaFormat::VideoMp4 {
                                            r.video_quality = self.main_video_quality;
                                        }
                                    }
                                }
                                self.recalculate_main_size();
                            }
                        }
                        MediaFormat::AudioMp3 => {
                            let prev_q = self.main_audio_quality;
                            egui::ComboBox::from_id_salt("main_aqual_combo")
                                .width(195.0)
                                .selected_text(self.main_audio_quality.label())
                                .show_ui(ui, |ui| {
                                    for q in AudioQuality::ALL {
                                        ui.selectable_value(
                                            &mut self.main_audio_quality,
                                            q,
                                            q.label(),
                                        );
                                    }
                                });
                            if self.main_audio_quality != prev_q {
                                if self.is_playlist_mode {
                                    for r in &mut self.playlist_rows {
                                        if r.format == MediaFormat::AudioMp3 {
                                            r.audio_quality = self.main_audio_quality;
                                        }
                                    }
                                }
                                self.recalculate_main_size();
                            }
                        }
                        MediaFormat::ImageGif => {
                            ui.add_enabled(
                                false,
                                egui::Button::new("No aplica (Original)")
                                    .min_size(egui::vec2(195.0, 26.0)),
                            );
                        }
                    }
                });

                Frame::new()
                    .fill(self.palette.bg_badge)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            RichText::new(&self.main_size_label)
                                .size(12.5)
                                .strong()
                                .color(self.palette.text_accent),
                        );
                    });
            });

            self.render_folder_row(ui);
        });

        ui.add_space(6.0);

        // Botón principal + Botón Cancelar
        let btn_text = if self.is_busy {
            self.busy_action_text.clone()
        } else if self.is_playlist_mode {
            let n = self.playlist_rows.iter().filter(|r| r.checked).count();
            if n > 0 {
                format!("DESCARGAR SELECCIONADOS ({n})")
            } else {
                "SELECCIONA AL MENOS 1 ELEMENTO".into()
            }
        } else {
            "INICIAR DESCARGA".into()
        };

        ui.horizontal(|ui| {
            let cancel_w = if self.is_busy { 130.0 } else { 0.0 };
            let main_w = (ui.available_width() - cancel_w).max(200.0);
            let zero_playlist_selected =
                self.is_playlist_mode && self.playlist_rows.iter().all(|r| !r.checked);
            let can_start_main = self.engines_ready && !self.is_busy && !zero_playlist_selected;
            ui.add_enabled_ui(can_start_main, |ui| {
                if self.styled_button(ui, &btn_text, self.palette.accent, egui::vec2(main_w, 42.0))
                {
                    self.start_main_download(ctx);
                }
            });
            if self.is_busy
                && self.styled_button(
                    ui,
                    "CANCELAR",
                    self.palette.btn_danger,
                    egui::vec2(122.0, 42.0),
                )
            {
                self.push_log("[AVISO] Cancelando descarga en curso...");
                self.cancel_token.cancel();
            }
        });
    }

    fn render_clip_tab(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        self.card_frame().show(ui, |ui| {
            // Cabecera de enlace
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Enlace del video a recortar (URL):")
                        .size(13.0)
                        .strong()
                        .color(self.palette.text_primary),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(&self.clip_media_info_text)
                            .size(11.5)
                            .color(self.palette.text_accent),
                    );
                });
            });

            // Input URL + botones
            ui.horizontal(|ui| {
                let btn_block_w = 290.0;
                let entry_w = (ui.available_width() - btn_block_w).max(200.0);
                let resp = ui.add_enabled(
                    !self.is_busy,
                    egui::TextEdit::singleline(&mut self.clip_url)
                        .hint_text("Pega el enlace del video que deseas recortar...")
                        .desired_width(entry_w),
                );
                if resp.changed() && self.clip_url != self.clip_last_edited_url {
                    self.clip_last_edited_url = self.clip_url.clone();
                    let trimmed = self.clip_url.trim();
                    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                        self.clip_debounce_deadline =
                            Some(Instant::now() + Duration::from_millis(300));
                    } else {
                        self.clip_debounce_deadline = None;
                    }
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.trigger_clip_analysis(ctx, false);
                }

                ui.add_enabled_ui(self.engines_ready && !self.is_busy, |ui| {
                    if self.styled_button(
                        ui,
                        "Pegar",
                        self.palette.btn_secondary,
                        egui::vec2(72.0, 32.0),
                    ) {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            if let Ok(text) = cb.get_text() {
                                let clean = text.trim().to_string();
                                if !clean.is_empty() {
                                    self.clip_url = clean.clone();
                                    self.clip_last_edited_url = clean;
                                    self.trigger_clip_analysis(ctx, true);
                                }
                            }
                        }
                    }
                    let load_lbl = if self.clip_loading {
                        "Cargando..."
                    } else {
                        "Cargar Video"
                    };
                    if ui
                        .add_enabled(
                            !self.clip_loading,
                            egui::Button::new(RichText::new(load_lbl).color(Color32::WHITE).strong())
                                .fill(self.palette.accent)
                                .min_size(egui::vec2(114.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.trigger_clip_analysis(ctx, true);
                    }
                    if self.styled_button(
                        ui,
                        "Limpiar",
                        self.palette.btn_secondary,
                        egui::vec2(74.0, 32.0),
                    ) {
                        self.clear_clip_url();
                    }
                });
            });

            // Tarjeta vista previa del clip
            if let Some(ref info) = self.clip_analyzed_info {
                let title = info
                    .title
                    .as_deref()
                    .unwrap_or("Video detectado")
                    .trim();
                let uploader = info
                    .uploader
                    .as_deref()
                    .or(info.channel.as_deref())
                    .or(info.extractor_key.as_deref())
                    .unwrap_or("")
                    .trim();
                let dur_str = format_duration(info.duration);
                let mut parts = Vec::new();
                if !uploader.is_empty() {
                    parts.push(uploader.chars().take(40).collect::<String>());
                }
                if info.duration.unwrap_or(0.0) > 0.0 {
                    parts.push(format!("Duración total: {dur_str}"));
                } else {
                    parts.push("Duración variable (puedes escribir Inicio y Fin manualmente)".into());
                }
                let title_short: String = title.chars().take(85).collect();
                let meta = parts.join("  •  ");
                let tex = self.clip_thumb_tex.clone();
                self.render_preview_card(ui, tex.as_ref(), &title_short, &meta);
            }

            // Caja de línea de tiempo dual sincronizada con tope físico independiente
            let max_limit = if self.clip_total_duration > 0.0 {
                self.clip_total_duration.max(1.0)
            } else {
                self.clip_end_sec.max(60.0)
            };
            let min_gap = if max_limit < 10.0 { 0.5 } else { 1.0 };

            self.subcard_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Fragmento a recortar (escribe MM:SS o desliza las barras):")
                            .size(12.0)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let clip_dur = (self.clip_end_sec - self.clip_start_sec).max(0.5);
                        let summary = format!(
                            "Rango: {} -> {}  (Duración del clip: {})",
                            format_timestamp_input(self.clip_start_sec),
                            format_timestamp_input(self.clip_end_sec),
                            format_duration(Some(clip_dur))
                        );
                        ui.label(
                            RichText::new(summary)
                                .size(12.0)
                                .strong()
                                .color(self.palette.text_accent),
                        );
                    });
                });

                ui.add_enabled_ui(!self.is_busy, |ui| {
                    // Inicio (Desde)
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [96.0, 26.0],
                            egui::Label::new(RichText::new("Inicio (Desde):").size(12.0).strong()),
                        );
                        let st_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.clip_start_text)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(74.0),
                        );
                        if st_resp.lost_focus()
                            || (st_resp.has_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            self.commit_clip_time_entries();
                        }

                        let mut slider_start = self.clip_start_sec;
                        let sl_w = ui.available_width().max(120.0);
                        ui.spacing_mut().slider_width = sl_w;
                        if ui
                            .add(
                                egui::Slider::new(&mut slider_start, 0.0..=max_limit)
                                    .show_value(false),
                            )
                            .changed()
                        {
                            let max_allowed = (self.clip_end_sec - min_gap).max(0.0);
                            let clamped = slider_start.clamp(0.0, max_allowed);
                            self.clip_start_sec = if max_limit < 10.0 {
                                (clamped * 10.0).round() / 10.0
                            } else {
                                clamped.round()
                            };
                            self.clip_start_text = format_timestamp_input(self.clip_start_sec);
                            self.recalculate_clip_size();
                        }
                    });

                    // Fin (Hasta)
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [96.0, 26.0],
                            egui::Label::new(RichText::new("Fin (Hasta):").size(12.0).strong()),
                        );
                        let end_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.clip_end_text)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(74.0),
                        );
                        if end_resp.lost_focus()
                            || (end_resp.has_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            self.commit_clip_time_entries();
                        }

                        let mut slider_end = self.clip_end_sec;
                        let sl_w = ui.available_width().max(120.0);
                        ui.spacing_mut().slider_width = sl_w;
                        if ui
                            .add(
                                egui::Slider::new(&mut slider_end, 0.0..=max_limit)
                                    .show_value(false),
                            )
                            .changed()
                        {
                            let min_allowed = max_limit.min(self.clip_start_sec + min_gap);
                            let clamped = slider_end.clamp(min_allowed, max_limit);
                            self.clip_end_sec = if max_limit < 10.0 {
                                (clamped * 10.0).round() / 10.0
                            } else {
                                clamped.round()
                            };
                            self.clip_end_text = format_timestamp_input(self.clip_end_sec);
                            self.recalculate_clip_size();
                        }
                    });
                });
            });

            // Selectores de Formato, Calidad y Peso del clip
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!self.is_busy, |ui| {
                    ui.label(RichText::new("Formato:").size(13.0).strong());
                    let prev_fmt = self.clip_format;
                    egui::ComboBox::from_id_salt("clip_format_combo")
                        .width(135.0)
                        .selected_text(self.clip_format.label())
                        .show_ui(ui, |ui| {
                            for f in MediaFormat::ALL {
                                ui.selectable_value(&mut self.clip_format, f, f.label());
                            }
                        });
                    if self.clip_format != prev_fmt {
                        self.recalculate_clip_size();
                    }

                    ui.label(RichText::new("Calidad:").size(13.0).strong());
                    match self.clip_format {
                        MediaFormat::VideoMp4 => {
                            let prev_q = self.clip_video_quality;
                            egui::ComboBox::from_id_salt("clip_vqual_combo")
                                .width(195.0)
                                .selected_text(self.clip_video_quality.label())
                                .show_ui(ui, |ui| {
                                    for q in VideoQuality::ALL {
                                        ui.selectable_value(
                                            &mut self.clip_video_quality,
                                            q,
                                            q.label(),
                                        );
                                    }
                                });
                            if self.clip_video_quality != prev_q {
                                self.recalculate_clip_size();
                            }
                        }
                        MediaFormat::AudioMp3 => {
                            let prev_q = self.clip_audio_quality;
                            egui::ComboBox::from_id_salt("clip_aqual_combo")
                                .width(195.0)
                                .selected_text(self.clip_audio_quality.label())
                                .show_ui(ui, |ui| {
                                    for q in AudioQuality::ALL {
                                        ui.selectable_value(
                                            &mut self.clip_audio_quality,
                                            q,
                                            q.label(),
                                        );
                                    }
                                });
                            if self.clip_audio_quality != prev_q {
                                self.recalculate_clip_size();
                            }
                        }
                        MediaFormat::ImageGif => {
                            let prev_q = self.clip_gif_quality;
                            egui::ComboBox::from_id_salt("clip_gqual_combo")
                                .width(195.0)
                                .selected_text(self.clip_gif_quality.label())
                                .show_ui(ui, |ui| {
                                    for q in GifQuality::ALL {
                                        ui.selectable_value(
                                            &mut self.clip_gif_quality,
                                            q,
                                            q.label(),
                                        );
                                    }
                                });
                            if self.clip_gif_quality != prev_q {
                                self.recalculate_clip_size();
                            }
                        }
                    }
                });

                Frame::new()
                    .fill(self.palette.bg_badge)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            RichText::new(&self.clip_size_label)
                                .size(12.5)
                                .strong()
                                .color(self.palette.text_accent),
                        );
                    });
            });

            self.render_folder_row(ui);
        });

        ui.add_space(6.0);

        let btn_text = if self.is_busy {
            self.busy_action_text.clone()
        } else {
            "RECORTAR Y DESCARGAR FRAGMENTO".into()
        };

        ui.horizontal(|ui| {
            let cancel_w = if self.is_busy { 130.0 } else { 0.0 };
            let main_w = (ui.available_width() - cancel_w).max(200.0);
            ui.add_enabled_ui(self.engines_ready && !self.is_busy, |ui| {
                if self.styled_button(ui, &btn_text, self.palette.accent, egui::vec2(main_w, 42.0))
                {
                    self.start_clip_download(ctx);
                }
            });
            if self.is_busy
                && self.styled_button(
                    ui,
                    "CANCELAR",
                    self.palette.btn_danger,
                    egui::vec2(122.0, 42.0),
                )
            {
                self.push_log("[AVISO] Cancelando recorte en curso...");
                self.cancel_token.cancel();
            }
        });
    }
}
