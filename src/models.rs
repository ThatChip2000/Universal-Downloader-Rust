use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const APP_VERSION: &str = "4.0.0";
pub const YTDLP_RELEASE_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
pub const YTDLP_PYPI_API: &str = "https://pypi.org/pypi/yt-dlp/json";
pub const YTDLP_EXE_DOWNLOAD_URL: &str =
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Main,
    Clip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
    System,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Dark, ThemeMode::Light, ThemeMode::System];

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Dark => "Oscuro",
            ThemeMode::Light => "Claro",
            ThemeMode::System => "Sistema",
        }
    }

    pub fn from_str_saved(s: &str) -> Self {
        match s.trim() {
            "Claro" | "light" => ThemeMode::Light,
            "Sistema" | "system" => ThemeMode::System,
            _ => ThemeMode::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFormat {
    VideoMp4,
    AudioMp3,
    ImageGif,
}

impl MediaFormat {
    pub const ALL: [MediaFormat; 3] = [
        MediaFormat::VideoMp4,
        MediaFormat::AudioMp3,
        MediaFormat::ImageGif,
    ];
    pub const PLAYLIST_OPTIONS: [MediaFormat; 2] = [MediaFormat::VideoMp4, MediaFormat::AudioMp3];

    pub fn label(self) -> &'static str {
        match self {
            MediaFormat::VideoMp4 => "Video MP4",
            MediaFormat::AudioMp3 => "Audio MP3",
            MediaFormat::ImageGif => "Imagen / GIF",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoQuality {
    BestAuto,
    Uhd4k,
    Qhd2k,
    FullHd1080,
    Hd720,
    Sd480,
    Low360,
}

impl VideoQuality {
    pub const ALL: [VideoQuality; 7] = [
        VideoQuality::BestAuto,
        VideoQuality::Uhd4k,
        VideoQuality::Qhd2k,
        VideoQuality::FullHd1080,
        VideoQuality::Hd720,
        VideoQuality::Sd480,
        VideoQuality::Low360,
    ];

    pub fn label(self) -> &'static str {
        match self {
            VideoQuality::BestAuto => "Mejor disponible (Auto)",
            VideoQuality::Uhd4k => "2160p (4K Ultra HD)",
            VideoQuality::Qhd2k => "1440p (2K QHD)",
            VideoQuality::FullHd1080 => "1080p (Full HD)",
            VideoQuality::Hd720 => "720p (HD)",
            VideoQuality::Sd480 => "480p (SD)",
            VideoQuality::Low360 => "360p (Datos reducidos)",
        }
    }

    pub fn short_label(self) -> &'static str {
        self.label().split('(').next().unwrap_or(self.label()).trim()
    }

    /// (alto_estandar, ancho_estandar) para soportar 16:9, 21:9 ultrawide y 9:16 vertical.
    pub fn tier_bounds(self) -> Option<(u32, u32)> {
        match self {
            VideoQuality::BestAuto => None,
            VideoQuality::Uhd4k => Some((2160, 3840)),
            VideoQuality::Qhd2k => Some((1440, 2560)),
            VideoQuality::FullHd1080 => Some((1080, 1920)),
            VideoQuality::Hd720 => Some((720, 1280)),
            VideoQuality::Sd480 => Some((480, 854)),
            VideoQuality::Low360 => Some((360, 640)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioQuality {
    Kbps320,
    Kbps256,
    Kbps192,
    Kbps128,
}

impl AudioQuality {
    pub const ALL: [AudioQuality; 4] = [
        AudioQuality::Kbps320,
        AudioQuality::Kbps256,
        AudioQuality::Kbps192,
        AudioQuality::Kbps128,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AudioQuality::Kbps320 => "320 kbps (Máxima)",
            AudioQuality::Kbps256 => "256 kbps (Alta)",
            AudioQuality::Kbps192 => "192 kbps (Estándar)",
            AudioQuality::Kbps128 => "128 kbps (Ligera)",
        }
    }

    pub fn short_label(self) -> &'static str {
        self.label().split('(').next().unwrap_or(self.label()).trim()
    }

    pub fn kbps(self) -> u32 {
        match self {
            AudioQuality::Kbps320 => 320,
            AudioQuality::Kbps256 => 256,
            AudioQuality::Kbps192 => 192,
            AudioQuality::Kbps128 => 128,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GifQuality {
    High480,
    Max720,
    Standard360,
    Original,
}

impl GifQuality {
    pub const ALL: [GifQuality; 4] = [
        GifQuality::High480,
        GifQuality::Max720,
        GifQuality::Standard360,
        GifQuality::Original,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GifQuality::High480 => "Alta (480p • 15 FPS)",
            GifQuality::Max720 => "Máxima (720p HD • 15 FPS)",
            GifQuality::Standard360 => "Estándar (360p • 12 FPS)",
            GifQuality::Original => "Original (15 FPS)",
        }
    }

    pub fn params(self) -> (Option<u32>, u32) {
        match self {
            GifQuality::High480 => (Some(480), 15),
            GifQuality::Max720 => (Some(720), 15),
            GifQuality::Standard360 => (Some(360), 12),
            GifQuality::Original => (None, 15),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ThumbnailItem {
    pub url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MediaFormatItem {
    pub format_id: Option<String>,
    pub url: Option<String>,
    pub ext: Option<String>,
    pub protocol: Option<String>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub tbr: Option<f64>,
    pub vbr: Option<f64>,
    pub abr: Option<f64>,
    pub filesize: Option<u64>,
    pub filesize_approx: Option<u64>,
    pub fragments: Option<Vec<serde_json::Value>>,
    pub http_headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct PlaylistEntry {
    pub id: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub url: Option<String>,
    pub webpage_url: Option<String>,
    pub original_url: Option<String>,
    pub thumbnail: Option<String>,
    pub thumbnails: Option<Vec<ThumbnailItem>>,
    pub formats: Option<Vec<MediaFormatItem>>,
    pub uploader: Option<String>,
    pub channel: Option<String>,
    pub extractor: Option<String>,
    pub extractor_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MediaInfo {
    pub id: Option<String>,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub channel: Option<String>,
    pub duration: Option<f64>,
    pub url: Option<String>,
    pub webpage_url: Option<String>,
    pub thumbnail: Option<String>,
    pub thumbnails: Option<Vec<ThumbnailItem>>,
    pub formats: Option<Vec<MediaFormatItem>>,
    pub filesize: Option<u64>,
    pub filesize_approx: Option<u64>,
    pub extractor: Option<String>,
    pub extractor_key: Option<String>,
    #[serde(rename = "_type")]
    pub result_type: Option<String>,
    pub entries: Option<Vec<PlaylistEntry>>,
    pub http_headers: Option<std::collections::HashMap<String, String>>,

    // Campos internos propios de Universal Downloader
    #[serde(skip)]
    pub is_playlist: bool,
    #[serde(skip)]
    pub normalized_entries: Vec<PlaylistEntry>,
    #[serde(skip)]
    pub suno_mp4_url: Option<String>,
    #[serde(skip)]
    pub suno_img_url: Option<String>,
    #[serde(skip)]
    pub tw_direct_fallback: bool,
    #[serde(skip)]
    pub tw_photo_urls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlaylistRowState {
    pub checked: bool,
    pub index: usize,
    pub title: String,
    pub url: String,
    pub duration: Option<f64>,
    pub format: MediaFormat,
    pub video_quality: VideoQuality,
    pub audio_quality: AudioQuality,
}

#[derive(Debug, Clone)]
pub struct SelectedStream {
    pub locked_id: String,
    pub best_v: MediaFormatItem,
    pub total_bytes: u64,
    pub is_exact: bool,
}

#[derive(Debug, Clone)]
pub struct ThumbnailPixels {
    pub url: String,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub enum UiMessage {
    Log(String),
    Status {
        text: String,
        progress: Option<f32>,
        percent: Option<String>,
    },
    Busy {
        busy: bool,
        action_text: Option<String>,
    },
    EnginesReady {
        ytdlp_path: PathBuf,
        ffmpeg_path: Option<PathBuf>,
        qjs_path: Option<PathBuf>,
        engine_version: String,
        config: crate::config::AppConfig,
    },
    UpdateAvailable {
        local_ver: String,
        latest_ver: String,
    },
    UpdateFinished {
        engine_version: String,
        config: crate::config::AppConfig,
    },
    MainAnalyzed {
        generation: u64,
        url: String,
        info: MediaInfo,
    },
    MainAnalyzeFailed {
        generation: u64,
        error: String,
    },
    ClipAnalyzed {
        generation: u64,
        url: String,
        info: MediaInfo,
    },
    ClipAnalyzeFailed {
        generation: u64,
        error: String,
    },
    ThumbnailLoaded {
        is_clip_tab: bool,
        generation: u64,
        pixels: ThumbnailPixels,
    },
    DownloadFinished {
        final_status: String,
        final_percent: String,
        progress: f32,
    },
}

pub fn normalize_version(ver_str: &str) -> Vec<u64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\d+").expect("regex valida"));
    let parts: Vec<u64> = re
        .find_iter(ver_str)
        .filter_map(|m| m.as_str().parse::<u64>().ok())
        .collect();
    if parts.is_empty() {
        vec![0]
    } else {
        parts
    }
}

pub fn format_bytes(num_bytes: u64, is_exact: bool) -> String {
    if num_bytes == 0 {
        return "No disponible".to_string();
    }
    let prefix = if is_exact { "" } else { "~ " };
    let b = num_bytes as f64;
    if num_bytes < 1024 * 1024 {
        format!("{prefix}{:.1} KB", b / 1024.0)
    } else if num_bytes < 1024 * 1024 * 1024 {
        format!("{prefix}{:.1} MB", b / (1024.0 * 1024.0))
    } else {
        format!("{prefix}{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_duration(seconds: Option<f64>) -> String {
    let Some(s) = seconds else {
        return "--:--".to_string();
    };
    if s <= 0.0 || !s.is_finite() {
        return "--:--".to_string();
    }
    let total_sec = s as u64;
    let hrs = total_sec / 3600;
    let rem = total_sec % 3600;
    let mins = rem / 60;
    let secs = rem % 60;
    if hrs > 0 {
        format!("{hrs:02}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

pub fn format_timestamp_input(seconds: f64) -> String {
    if seconds < 0.0 || !seconds.is_finite() {
        return "00:00".to_string();
    }
    let total_sec = seconds.round() as u64;
    let hrs = total_sec / 3600;
    let rem = total_sec % 3600;
    let mins = rem / 60;
    let secs = rem % 60;
    if hrs > 0 {
        format!("{hrs:02}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

pub fn parse_timestamp_input(text: &str) -> Option<f64> {
    let s = text.trim().replace(',', ".");
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    let val = match parts.len() {
        1 => parts[0].trim().parse::<f64>().ok()?,
        2 => {
            let m = parts[0].trim().parse::<i64>().ok()?;
            let sec = parts[1].trim().parse::<f64>().ok()?;
            (m as f64) * 60.0 + sec
        }
        3 => {
            let h = parts[0].trim().parse::<i64>().ok()?;
            let m = parts[1].trim().parse::<i64>().ok()?;
            let sec = parts[2].trim().parse::<f64>().ok()?;
            (h as f64) * 3600.0 + (m as f64) * 60.0 + sec
        }
        _ => return None,
    };
    if val >= 0.0 && val.is_finite() {
        Some(val)
    } else {
        None
    }
}

pub fn clean_ansi(text: &str) -> String {
    static ANSI_RE: OnceLock<Regex> = OnceLock::new();
    let re = ANSI_RE.get_or_init(|| {
        Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").expect("regex ansi")
    });
    re.replace_all(text, "").into_owned()
}

pub fn sanitize_filename(raw: &str, max_chars: usize) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || (c as u32) < 0x20
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = replaced.trim_matches(|c| c == ' ' || c == '.');
    let out: String = trimmed.chars().take(max_chars).collect();
    if out.is_empty() {
        "Descarga".to_string()
    } else {
        out
    }
}

pub fn is_suno_url(url: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^https?://(?:www\.)?(?:suno\.com|suno\.ai|cdn[0-9]*\.suno\.ai)/")
            .expect("regex suno")
    });
    re.is_match(url.trim())
}

pub fn is_twitter_status_url(url: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)https?://(?:www\.|mobile\.)?(?:x\.com|twitter\.com)/(?:[^/]+/status|i/status|i/web/status)/\d+",
        )
        .expect("regex twitter")
    });
    re.is_match(url.trim())
}

pub fn is_twitter_gif_info(info: &MediaInfo) -> bool {
    if let Some(formats) = &info.formats {
        if formats
            .iter()
            .any(|f| f.url.as_deref().unwrap_or("").contains("/tweet_video/"))
        {
            return true;
        }
    }
    if let Some(thumbs) = &info.thumbnails {
        if thumbs
            .iter()
            .any(|t| t.url.as_deref().unwrap_or("").contains("/tweet_video_thumb/"))
        {
            return true;
        }
    }
    info.url.as_deref().unwrap_or("").contains("/tweet_video/")
}

pub fn has_video(fmt: &MediaFormatItem) -> bool {
    if fmt.vcodec.as_deref() == Some("none") || fmt.ext.as_deref() == Some("mhtml") {
        return false;
    }
    fmt.vcodec.is_some()
        || fmt.height.unwrap_or(0) > 0
        || fmt.width.unwrap_or(0) > 0
        || fmt.vbr.unwrap_or(0.0) > 0.0
}

pub fn is_audio_only(fmt: &MediaFormatItem) -> bool {
    fmt.vcodec.as_deref() == Some("none") && fmt.acodec.as_deref() != Some("none")
}

pub fn has_exact_filesize(fmt: &MediaFormatItem) -> bool {
    let sz = fmt.filesize.unwrap_or(0);
    let proto = fmt.protocol.as_deref().unwrap_or("");
    sz > 0 && !proto.contains("m3u8")
}

pub fn stream_bytes(fmt: &MediaFormatItem, duration: Option<f64>) -> u64 {
    let exact = fmt.filesize.or(fmt.filesize_approx).unwrap_or(0);
    if exact > 0 {
        return exact;
    }
    let tbr = fmt
        .tbr
        .unwrap_or_else(|| fmt.vbr.unwrap_or(0.0) + fmt.abr.unwrap_or(0.0));
    if tbr > 0.0 {
        if let Some(dur) = duration {
            if dur > 0.0 {
                let proto = fmt.protocol.as_deref().unwrap_or("");
                let url_str = fmt.url.as_deref().unwrap_or("");
                // En manifiestos m3u8 de YouTube, 'tbr' es el pico máximo (~2.8x el promedio real VBR);
                // en X/Twitter (twimg.com), el bitrate reportado corresponde al promedio real.
                let factor = if proto.contains("m3u8") && !url_str.contains("twimg.com") {
                    0.36
                } else {
                    1.0
                };
                return ((tbr * factor * 1000.0 / 8.0) * dur) as u64;
            }
        }
    }
    0
}

pub fn fits_resolution_tier(fmt: &MediaFormatItem, bounds: Option<(u32, u32)>) -> bool {
    let Some((target_short, target_long)) = bounds else {
        return true;
    };
    let w = fmt.width.unwrap_or(0);
    let h = fmt.height.unwrap_or(0);
    if w > 0 && h > 0 {
        let short_side = w.min(h);
        let long_side = w.max(h);
        return short_side <= target_short && long_side <= target_long;
    }
    if h > 0 {
        return h <= target_short;
    }
    false
}

pub fn resolution_label(fmt: &MediaFormatItem) -> String {
    let w = fmt.width.unwrap_or(0);
    let h = fmt.height.unwrap_or(0);
    let long_side = w.max(h);
    if long_side >= 3500 || h >= 2000 {
        "2160p 4K".to_string()
    } else if long_side >= 2400 || h >= 1350 {
        "1440p 2K".to_string()
    } else if long_side >= 1800 || h >= 1000 {
        "1080p".to_string()
    } else if long_side >= 1200 || h >= 680 {
        "720p".to_string()
    } else if long_side >= 800 || h >= 450 {
        "480p".to_string()
    } else if h > 0 {
        format!("{h}p")
    } else {
        "Auto".to_string()
    }
}

pub fn parse_cli_progress(line: &str) -> Option<(f32, String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\[download\]\s+(?P<pct>\d+(?:\.\d+)?)%\s+of\s+.+?\s+at\s+(?P<speed>.+?)\s+ETA\s+(?P<eta>\S+)",
        )
        .expect("regex progress")
    });
    let caps = re.captures(line)?;
    let pct = caps.name("pct")?.as_str().parse::<f32>().ok()?;
    let speed = caps.name("speed")?.as_str().trim().to_string();
    let eta = caps.name("eta")?.as_str().trim().to_string();
    Some((pct, speed, eta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_version() {
        assert!(normalize_version("2026.10.26") > normalize_version("2026.10.21"));
        assert_eq!(normalize_version("v4.0.0"), vec![4, 0, 0]);
        assert_eq!(normalize_version(""), vec![0]);
    }

    #[test]
    fn test_timestamps() {
        assert_eq!(parse_timestamp_input("01:30"), Some(90.0));
        assert_eq!(parse_timestamp_input("02:05:10"), Some(7510.0));
        assert_eq!(parse_timestamp_input("45,5"), Some(45.5));
        assert_eq!(parse_timestamp_input("-5"), None);
        assert_eq!(format_timestamp_input(0.0), "00:00");
        assert_eq!(format_timestamp_input(3665.0), "01:01:05");
    }

    #[test]
    fn test_fits_resolution_tier() {
        let f_1080 = MediaFormatItem {
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        };
        let f_vert = MediaFormatItem {
            width: Some(1080),
            height: Some(1920),
            ..Default::default()
        };
        let f_4k = MediaFormatItem {
            width: Some(3840),
            height: Some(2160),
            ..Default::default()
        };
        assert!(fits_resolution_tier(&f_1080, Some((1080, 1920))));
        assert!(fits_resolution_tier(&f_vert, Some((1080, 1920))));
        assert!(!fits_resolution_tier(&f_4k, Some((1080, 1920))));
    }

    #[test]
    fn test_stream_bytes_twitter_vs_youtube() {
        let tw_m3u8 = MediaFormatItem {
            url: Some("https://video.twimg.com/amplify_video/1/pl/a.m3u8".into()),
            protocol: Some("m3u8_native".into()),
            tbr: Some(3000.0),
            ..Default::default()
        };
        let yt_m3u8 = MediaFormatItem {
            url: Some("https://manifest.googlevideo.com/api/manifest/hls/a.m3u8".into()),
            protocol: Some("m3u8_native".into()),
            tbr: Some(3000.0),
            ..Default::default()
        };
        assert_eq!(stream_bytes(&tw_m3u8, Some(100.0)), 37_500_000);
        assert_eq!(
            stream_bytes(&yt_m3u8, Some(100.0)),
            (37_500_000.0 * 0.36) as u64
        );
    }

    #[test]
    fn test_format_bytes_and_urls() {
        assert_eq!(format_bytes(0, false), "No disponible");
        assert_eq!(format_bytes(1024 * 512, true), "512.0 KB");
        assert_eq!(format_bytes(1024 * 1024 * 15, false), "~ 15.0 MB");
        assert!(is_suno_url("https://suno.com/song/1234"));
        assert!(is_twitter_status_url(
            "https://x.com/CineEnCasaMX/status/2121731734530117650/video/1"
        ));
    }

    #[test]
    fn test_parse_cli_progress_variants() {
        assert_eq!(
            parse_cli_progress("[download]   0.5% of  614.43KiB at    2.42MiB/s ETA 00:00"),
            Some((0.5, "2.42MiB/s".to_string(), "00:00".to_string()))
        );
        assert_eq!(
            parse_cli_progress("[download]   0.2% of  614.43KiB at  Unknown B/s ETA Unknown"),
            Some((0.2, "Unknown B/s".to_string(), "Unknown".to_string()))
        );
        assert_eq!(
            parse_cli_progress("[download]  45.2% of ~  20.40MiB at    3.50MiB/s ETA 00:04"),
            Some((45.2, "3.50MiB/s".to_string(), "00:04".to_string()))
        );
        assert_eq!(
            parse_cli_progress("[download]  50.0% of ~ 12.34MiB at  1.23MiB/s ETA 00:05 (frag 6/12)"),
            Some((50.0, "1.23MiB/s".to_string(), "00:05".to_string()))
        );
        assert_eq!(
            parse_cli_progress("[download]  12.5% of Unknown size at  1.20MiB/s ETA Unknown"),
            Some((12.5, "1.20MiB/s".to_string(), "Unknown".to_string()))
        );
        assert_eq!(
            parse_cli_progress("[download] Destination: C:\\video.mp4"),
            None
        );
    }
}
