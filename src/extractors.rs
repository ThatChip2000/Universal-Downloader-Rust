use crate::engines::{apply_no_window, ytdlp_js_cli_args, CancelToken};
use crate::models::{
    fits_resolution_tier, has_exact_filesize, has_video, is_audio_only, is_suno_url,
    is_twitter_status_url, stream_bytes, MediaFormatItem, MediaInfo, PlaylistEntry, SelectedStream,
    ThumbnailItem, ThumbnailPixels, VideoQuality, APP_VERSION, DEFAULT_USER_AGENT,
};
use image::imageops::FilterType;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE, USER_AGENT};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

pub fn build_http_client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(DEFAULT_USER_AGENT)
        .build()
        .map_err(|e| format!("Error iniciando cliente HTTP: {e}"))
}

fn resolve_entry_url(entry: &PlaylistEntry) -> Option<String> {
    for cand in [
        entry.webpage_url.as_deref(),
        entry.url.as_deref(),
        entry.original_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if cand.starts_with("http://") || cand.starts_with("https://") {
            return Some(cand.to_string());
        }
    }
    if let Some(vid_id) = entry.id.as_deref() {
        if !vid_id.is_empty() {
            return Some(format!("https://www.youtube.com/watch?v={vid_id}"));
        }
    }
    None
}

fn run_ytdlp_dump_json(
    url: &str,
    ytdlp_path: &Path,
    qjs_path: Option<&Path>,
    flat_playlist: bool,
    cancel: Option<&CancelToken>,
) -> Result<MediaInfo, String> {
    if !ytdlp_path.is_file() {
        return Err("No se encontró yt-dlp.exe para analizar el enlace.".to_string());
    }
    if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
        return Err("Descarga cancelada por el usuario.".to_string());
    }

    let mut cmd = Command::new(ytdlp_path);
    cmd.args(["--dump-single-json", "--no-warnings", "--encoding", "utf-8"]);
    if flat_playlist {
        cmd.args(["--flat-playlist", "--playlist-end", "100"]);
    } else {
        cmd.arg("--no-playlist");
    }
    cmd.args(ytdlp_js_cli_args(qjs_path));
    cmd.arg(url);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    apply_no_window(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Error ejecutando yt-dlp.exe: {e}"))?;
    if let Some(tok) = cancel {
        tok.set_pid(Some(child.id()));
    }

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let out_handle = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut so) = stdout_pipe {
            let _ = so.read_to_end(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut se) = stderr_pipe {
            let _ = se.read_to_end(&mut buf);
        }
        buf
    });

    let status = child
        .wait()
        .map_err(|e| format!("Error esperando a yt-dlp.exe: {e}"))?;
    if let Some(tok) = cancel {
        tok.set_pid(None);
    }

    let stdout_bytes = out_handle.join().unwrap_or_default();
    let stderr_bytes = err_handle.join().unwrap_or_default();

    if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
        return Err("Descarga cancelada por el usuario.".to_string());
    }

    if !status.success() {
        let err_str = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
        let out_str = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
        return Err(if !err_str.is_empty() {
            err_str
        } else {
            out_str
        });
    }

    let stdout_str = String::from_utf8_lossy(&stdout_bytes);
    let json_line = stdout_str
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| "yt-dlp no devolvió JSON válido.".to_string())?;

    serde_json::from_str::<MediaInfo>(json_line)
        .map_err(|e| format!("Error parseando metadatos JSON: {e}"))
}

pub fn extract_suno_info(url: &str) -> Result<MediaInfo, String> {
    let client = build_http_client(12)?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("No se pudo consultar Suno: {e}"))?;
    let final_url = resp.url().to_string();
    let html = resp
        .text()
        .map_err(|e| format!("Error leyendo respuesta de Suno: {e}"))?;

    static UUID_RE: OnceLock<Regex> = OnceLock::new();
    let uuid_re = UUID_RE.get_or_init(|| {
        Regex::new(r"([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})")
            .expect("uuid regex")
    });

    let combined = format!("{final_url} {html}");
    let song_id = uuid_re
        .captures(&combined)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| "No se encontró el identificador de la pista en el enlace.".to_string())?;

    let mut title = format!("Suno_{}", &song_id[..8.min(song_id.len())]);
    static TITLE_RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = TITLE_RES.get_or_init(|| {
        vec![
            Regex::new(r#"(?i)<meta[^>]+property=["']og:title["'][^>]+content=["']([^"']+)["']"#)
                .unwrap(),
            Regex::new(r#"(?i)<meta[^>]+content=["']([^"']+)["'][^>]+property=["']og:title["']"#)
                .unwrap(),
            Regex::new(r"(?i)<title>([^<]+)</title>").unwrap(),
        ]
    });
    static SUFFIX_RE: OnceLock<Regex> = OnceLock::new();
    let suffix_re = SUFFIX_RE.get_or_init(|| Regex::new(r"(?i)\s*\|\s*Suno.*$").unwrap());

    for pat in patterns {
        if let Some(caps) = pat.captures(&html) {
            if let Some(m) = caps.get(1) {
                let cleaned = suffix_re.replace_all(m.as_str().trim(), "").trim().to_string();
                if !cleaned.is_empty() {
                    title = cleaned;
                    break;
                }
            }
        }
    }

    let mp4_url = format!("https://cdn1.suno.ai/{song_id}.mp4");
    static IMG_RE: OnceLock<Regex> = OnceLock::new();
    let img_re = IMG_RE
        .get_or_init(|| Regex::new(r#"https://cdn[0-9]*\.suno\.ai/image_large_[^"'\s<>]+"#).unwrap());
    let img_url = img_re
        .find(&html)
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| format!("https://cdn2.suno.ai/image_large_{song_id}.jpeg"));

    // Tamaño exacto por petición parcial Range: bytes=0-0
    let mut exact_bytes: u64 = 0;
    if let Ok(r_range) = client.get(&mp4_url).header(RANGE, "bytes=0-0").send() {
        if let Some(cr) = r_range.headers().get("Content-Range").and_then(|v| v.to_str().ok()) {
            if let Some((_, total_str)) = cr.rsplit_once('/') {
                if let Ok(sz) = total_str.trim().parse::<u64>() {
                    exact_bytes = sz;
                }
            }
        }
    }

    static DUR_RE: OnceLock<Regex> = OnceLock::new();
    let dur_re = DUR_RE.get_or_init(|| Regex::new(r#"\\?"duration\\?"\s*:\s*(\d+(?:\.\d+)?)"#).unwrap());
    let duration = dur_re
        .captures(&html)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok());

    let fmt = MediaFormatItem {
        format_id: Some("suno-mp4".into()),
        url: Some(mp4_url.clone()),
        ext: Some("mp4".into()),
        protocol: Some("https".into()),
        vcodec: Some("h264".into()),
        acodec: Some("aac".into()),
        width: Some(1080),
        height: Some(1920),
        filesize: (exact_bytes > 0).then_some(exact_bytes),
        filesize_approx: (exact_bytes > 0).then_some(exact_bytes),
        ..Default::default()
    };

    Ok(MediaInfo {
        id: Some(song_id),
        title: Some(title),
        uploader: Some("Audio HD".into()),
        duration,
        thumbnail: Some(img_url.clone()),
        thumbnails: Some(vec![ThumbnailItem {
            url: Some(img_url.clone()),
            width: None,
            height: None,
        }]),
        webpage_url: Some(url.to_string()),
        extractor: Some("suno_direct".into()),
        formats: Some(vec![fmt]),
        filesize: (exact_bytes > 0).then_some(exact_bytes),
        suno_mp4_url: Some(mp4_url),
        suno_img_url: Some(img_url),
        ..Default::default()
    })
}

pub fn extract_twitter_fallback_info(url: &str) -> Result<Option<MediaInfo>, String> {
    static TW_RE: OnceLock<Regex> = OnceLock::new();
    let tw_re = TW_RE.get_or_init(|| {
        Regex::new(
            r"(?i)https?://(?:www\.|mobile\.)?(?:x\.com|twitter\.com)/(?:[^/]+/status|i/status|i/web/status)/(\d+)(?:/video/(\d+))?",
        )
        .unwrap()
    });
    let Some(caps) = tw_re.captures(url) else {
        return Ok(None);
    };
    let tweet_id = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let vid_idx = caps
        .get(2)
        .and_then(|m| m.as_str().parse::<usize>().ok())
        .map(|n| n.saturating_sub(1))
        .unwrap_or(0);

    let api_url = format!("https://api.fxtwitter.com/status/{tweet_id}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(format!("UniversalDownloader/{APP_VERSION}"))
        .build()
        .map_err(|e| format!("Error creando cliente FxTwitter: {e}"))?;

    let resp = client
        .get(&api_url)
        .send()
        .map_err(|e| format!("Error consultando FxTwitter: {e}"))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let data: Value = resp
        .json()
        .map_err(|e| format!("JSON inválido de FxTwitter: {e}"))?;

    let tweet = data.get("tweet").cloned().unwrap_or(Value::Null);
    let media_block = tweet.get("media").cloned().unwrap_or(Value::Null);

    let photo_urls: Vec<String> = media_block
        .get("photos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let videos = media_block
        .get("videos")
        .and_then(|v| v.as_array())
        .or_else(|| media_block.get("all").and_then(|v| v.as_array()))
        .cloned()
        .unwrap_or_default();

    let author_obj = tweet.get("author").cloned().unwrap_or(Value::Null);
    let uploader = author_obj
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| author_obj.get("screen_name").and_then(|v| v.as_str()))
        .unwrap_or("X")
        .to_string();

    static TCO_RE: OnceLock<Regex> = OnceLock::new();
    let tco_re = TCO_RE.get_or_init(|| Regex::new(r"https?://t\.co/\S+").unwrap());
    let raw_text = tweet.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
    let clean_title = tco_re.replace_all(raw_text, "").trim().to_string();
    let title = if clean_title.is_empty() {
        format!("{uploader} - Video {tweet_id}")
    } else {
        format!("{uploader} - {clean_title}")
    };

    if videos.is_empty() {
        if !photo_urls.is_empty() {
            return Ok(Some(MediaInfo {
                id: Some(tweet_id.to_string()),
                title: Some(title.chars().take(120).collect()),
                uploader: Some(uploader),
                thumbnail: Some(photo_urls[0].clone()),
                thumbnails: Some(vec![ThumbnailItem {
                    url: Some(photo_urls[0].clone()),
                    width: None,
                    height: None,
                }]),
                webpage_url: Some(url.to_string()),
                extractor: Some("twitter".into()),
                extractor_key: Some("Twitter".into()),
                tw_direct_fallback: true,
                tw_photo_urls: photo_urls,
                ..Default::default()
            }));
        }
        return Ok(None);
    }

    let vid = &videos[vid_idx.min(videos.len() - 1)];
    let duration = vid.get("duration").and_then(|v| v.as_f64());
    let thumb_url = vid
        .get("thumbnail_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let vid_type = vid
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("video")
        .to_lowercase();
    let vid_url_str = vid.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let is_gif = vid_type == "gif" || vid_url_str.contains("tweet_video");

    static RES_RE: OnceLock<Regex> = OnceLock::new();
    let res_re = RES_RE.get_or_init(|| Regex::new(r"/(\d+)x(\d+)/").unwrap());

    let mut formats = Vec::new();
    let raw_variants = vid
        .get("formats")
        .and_then(|v| v.as_array())
        .or_else(|| vid.get("variants").and_then(|v| v.as_array()))
        .cloned()
        .unwrap_or_default();

    let fallback_w = vid.get("width").and_then(|v| v.as_u64()).map(|n| n as u32);
    let fallback_h = vid.get("height").and_then(|v| v.as_u64()).map(|n| n as u32);

    for (idx, var) in raw_variants.iter().enumerate() {
        let v_url = var.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if !v_url.starts_with("http://") && !v_url.starts_with("https://") {
            continue;
        }
        if v_url.contains(".m3u8") || var.get("container").and_then(|v| v.as_str()) == Some("m3u8") {
            formats.push(MediaFormatItem {
                format_id: Some(format!("hls-master-{idx}")),
                url: Some(v_url.to_string()),
                ext: Some("mp4".into()),
                protocol: Some("m3u8_native".into()),
                vcodec: Some("h264".into()),
                acodec: Some(if is_gif { "none" } else { "aac" }.into()),
                width: fallback_w,
                height: fallback_h,
                ..Default::default()
            });
            continue;
        }

        let (w, h) = if let Some(rc) = res_re.captures(v_url) {
            (
                rc.get(1).and_then(|m| m.as_str().parse::<u32>().ok()),
                rc.get(2).and_then(|m| m.as_str().parse::<u32>().ok()),
            )
        } else {
            (fallback_w, fallback_h)
        };

        let bitrate = var.get("bitrate").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let tbr = if bitrate > 0.0 {
            Some((bitrate / 1000.0 * 100.0).round() / 100.0)
        } else {
            None
        };
        let fmt_id = match tbr {
            Some(t) => format!("http-{}", t as u64),
            None => format!("mp4-{idx}"),
        };

        formats.push(MediaFormatItem {
            format_id: Some(fmt_id),
            url: Some(v_url.to_string()),
            ext: Some("mp4".into()),
            protocol: Some("https".into()),
            vcodec: Some("h264".into()),
            acodec: Some(if is_gif { "none" } else { "aac" }.into()),
            width: w,
            height: h,
            tbr,
            ..Default::default()
        });
    }

    if formats.is_empty() && !vid_url_str.is_empty() {
        formats.push(MediaFormatItem {
            format_id: Some("http-best".into()),
            url: Some(vid_url_str.to_string()),
            ext: Some("mp4".into()),
            protocol: Some("https".into()),
            vcodec: Some("h264".into()),
            acodec: Some(if is_gif { "none" } else { "aac" }.into()),
            width: fallback_w,
            height: fallback_h,
            ..Default::default()
        });
    }

    if formats.is_empty() {
        return Ok(None);
    }

    let mut info = MediaInfo {
        id: Some(
            vid.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(tweet_id)
                .to_string(),
        ),
        title: Some(title.chars().take(120).collect()),
        uploader: Some(uploader),
        duration,
        thumbnail: thumb_url.clone(),
        thumbnails: thumb_url.map(|u| {
            vec![ThumbnailItem {
                url: Some(u),
                width: None,
                height: None,
            }]
        }),
        webpage_url: Some(url.to_string()),
        extractor: Some("twitter".into()),
        extractor_key: Some("Twitter".into()),
        formats: Some(formats),
        tw_direct_fallback: true,
        tw_photo_urls: photo_urls,
        ..Default::default()
    };

    populate_missing_direct_filesizes(&mut info);
    Ok(Some(info))
}

pub fn populate_missing_direct_filesizes(info: &mut MediaInfo) {
    let Some(formats) = info.formats.as_mut() else {
        return;
    };
    if formats.is_empty() {
        return;
    }

    let base_headers = info.http_headers.clone().unwrap_or_default();
    let probe_indices: Vec<usize> = formats
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            let proto = f.protocol.as_deref().unwrap_or("");
            let url_ok = f
                .url
                .as_deref()
                .map(|u| u.starts_with("http://") || u.starts_with("https://"))
                .unwrap_or(false);
            f.filesize.unwrap_or(0) == 0
                && (proto == "http" || proto == "https")
                && f.fragments.as_ref().map(|v| v.is_empty()).unwrap_or(true)
                && url_ok
        })
        .map(|(i, _)| i)
        .collect();

    if !probe_indices.is_empty() {
        if let Ok(client) = build_http_client(4) {
            let jobs: Vec<(usize, String, HeaderMap)> = probe_indices
                .iter()
                .filter_map(|&idx| {
                    let fmt = &formats[idx];
                    let target_url = fmt.url.clone()?;
                    let mut hmap = HeaderMap::new();
                    hmap.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
                    for (k, v) in &base_headers {
                        if let (Ok(hn), Ok(hv)) =
                            (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v))
                        {
                            hmap.insert(hn, hv);
                        }
                    }
                    if let Some(fh) = &fmt.http_headers {
                        for (k, v) in fh {
                            if let (Ok(hn), Ok(hv)) =
                                (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v))
                            {
                                hmap.insert(hn, hv);
                            }
                        }
                    }
                    Some((idx, target_url, hmap))
                })
                .collect();

            let results: Vec<(usize, u64)> = std::thread::scope(|s| {
                let handles: Vec<_> = jobs
                    .into_iter()
                    .map(|(idx, target_url, hmap)| {
                        let client = &client;
                        s.spawn(move || {
                            if let Ok(resp) = client.head(&target_url).headers(hmap.clone()).send()
                            {
                                if let Some(cl) = resp
                                    .headers()
                                    .get("Content-Length")
                                    .and_then(|v| v.to_str().ok())
                                {
                                    if let Ok(sz) = cl.trim().parse::<u64>() {
                                        if sz > 0 {
                                            return (idx, sz);
                                        }
                                    }
                                }
                            }
                            if let Ok(resp) = client
                                .get(&target_url)
                                .headers(hmap)
                                .header(RANGE, "bytes=0-0")
                                .send()
                            {
                                if let Some(cr) = resp
                                    .headers()
                                    .get("Content-Range")
                                    .and_then(|v| v.to_str().ok())
                                {
                                    if let Some((_, total_str)) = cr.rsplit_once('/') {
                                        if let Ok(sz) = total_str.trim().parse::<u64>() {
                                            if sz > 0 {
                                                return (idx, sz);
                                            }
                                        }
                                    }
                                }
                                if let Some(cl) = resp
                                    .headers()
                                    .get("Content-Length")
                                    .and_then(|v| v.to_str().ok())
                                {
                                    if let Ok(sz) = cl.trim().parse::<u64>() {
                                        if sz > 1 {
                                            return (idx, sz);
                                        }
                                    }
                                }
                            }
                            (idx, 0)
                        })
                    })
                    .collect();

                handles
                    .into_iter()
                    .filter_map(|h| h.join().ok())
                    .filter(|(_, sz)| *sz > 0)
                    .collect()
            });

            for (idx, exact_sz) in results {
                formats[idx].filesize = Some(exact_sz);
                formats[idx].filesize_approx = Some(exact_sz);
            }
        }
    }

    // Respaldo HLS bitrate para X/Twitter si HEAD/Range fue bloqueado
    if let Some(dur) = info.duration {
        if dur > 0.0 {
            let hls_audio_tbr = formats
                .iter()
                .filter(|f| is_audio_only(f))
                .filter_map(|f| f.tbr)
                .fold(0.0_f64, f64::max);
            let hls_audio_tbr = if hls_audio_tbr > 0.0 {
                hls_audio_tbr
            } else {
                128.0
            };

            let mut hls_by_res: HashMap<(u32, u32), f64> = HashMap::new();
            for f in formats.iter() {
                if f.protocol.as_deref().unwrap_or("").contains("m3u8") && has_video(f) {
                    if let Some(tbr) = f.tbr {
                        hls_by_res.insert((f.width.unwrap_or(0), f.height.unwrap_or(0)), tbr);
                    }
                }
            }

            for f in formats.iter_mut() {
                if f.filesize.unwrap_or(0) == 0
                    && f.format_id.as_deref().unwrap_or("").starts_with("http-")
                {
                    let key = (f.width.unwrap_or(0), f.height.unwrap_or(0));
                    if let Some(v_tbr) = hls_by_res.get(&key) {
                        let real_tbr = *v_tbr + hls_audio_tbr;
                        f.filesize_approx = Some(((real_tbr * 1000.0 / 8.0) * dur) as u64);
                    }
                }
            }
        }
    }
}

pub fn analyze_url(
    url: &str,
    ytdlp_path: &Path,
    qjs_path: Option<&Path>,
    allow_playlist: bool,
    cancel: Option<&CancelToken>,
) -> Result<MediaInfo, String> {
    if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
        return Err("Descarga cancelada por el usuario.".to_string());
    }
    if is_suno_url(url) {
        return extract_suno_info(url);
    }

    let mut info_res = run_ytdlp_dump_json(url, ytdlp_path, qjs_path, allow_playlist, cancel);

    if let Ok(ref mut info) = info_res {
        if allow_playlist
            && (info.entries.is_some() || info.result_type.as_deref() == Some("playlist"))
        {
            let entries_list: Vec<PlaylistEntry> = info.entries.clone().unwrap_or_default();
            if entries_list.len() > 1 {
                let mut normalized = Vec::new();
                for (idx, entry) in entries_list.into_iter().enumerate() {
                    let Some(resolved_u) = resolve_entry_url(&entry) else {
                        continue;
                    };
                    normalized.push(PlaylistEntry {
                        id: Some(
                            entry
                                .id
                                .unwrap_or_else(|| format!("item_{}", idx + 1)),
                        ),
                        title: Some(
                            entry
                                .title
                                .unwrap_or_else(|| format!("Elemento #{}", idx + 1)),
                        ),
                        duration: entry.duration,
                        url: Some(resolved_u),
                        thumbnail: entry.thumbnail,
                        thumbnails: entry.thumbnails,
                        ..Default::default()
                    });
                }
                if normalized.len() > 1 {
                    info.is_playlist = true;
                    info.normalized_entries = normalized;
                    return Ok(info.clone());
                } else if normalized.len() == 1 {
                    let single_u = normalized[0].url.clone().unwrap_or_else(|| url.to_string());
                    let mut single_info =
                        run_ytdlp_dump_json(&single_u, ytdlp_path, qjs_path, false, cancel)?;
                    populate_missing_direct_filesizes(&mut single_info);
                    return Ok(single_info);
                }
            } else if entries_list.len() == 1 {
                let first = &entries_list[0];
                if let Some(fmts) = &first.formats {
                    if !fmts.is_empty() {
                        let mut single_info = MediaInfo {
                            id: first.id.clone(),
                            title: first.title.clone(),
                            duration: first.duration,
                            url: first.url.clone(),
                            webpage_url: first.webpage_url.clone().or_else(|| Some(url.into())),
                            thumbnail: first.thumbnail.clone(),
                            thumbnails: first.thumbnails.clone(),
                            formats: Some(fmts.clone()),
                            uploader: first.uploader.clone(),
                            channel: first.channel.clone(),
                            extractor: first.extractor.clone(),
                            extractor_key: first.extractor_key.clone(),
                            ..Default::default()
                        };
                        populate_missing_direct_filesizes(&mut single_info);
                        return Ok(single_info);
                    }
                }
                let target_u = resolve_entry_url(first).unwrap_or_else(|| url.to_string());
                let mut single_info =
                    run_ytdlp_dump_json(&target_u, ytdlp_path, qjs_path, false, cancel)?;
                populate_missing_direct_filesizes(&mut single_info);
                return Ok(single_info);
            }
        }
        populate_missing_direct_filesizes(info);
        return Ok(info.clone());
    }

    if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
        return Err("Descarga cancelada por el usuario.".to_string());
    }

    if is_twitter_status_url(url) {
        if let Ok(Some(fb)) = extract_twitter_fallback_info(url) {
            return Ok(fb);
        }
    }

    Err(info_res.err().unwrap_or_else(|| "Error analizando URL".into()))
}

pub fn select_optimal_video_format(
    info: &MediaInfo,
    selected_quality: VideoQuality,
    url_hint: &str,
    has_ffmpeg: bool,
) -> Option<SelectedStream> {
    let duration = info.duration;
    let formats = info.formats.as_deref()?;
    let tier_bounds = selected_quality.tier_bounds();

    let all_video: Vec<MediaFormatItem> = formats
        .iter()
        .filter(|f| has_video(f) && f.format_id.as_deref().map(|s| !s.is_empty()).unwrap_or(false))
        .cloned()
        .collect();
    if all_video.is_empty() {
        return None;
    }

    let mut tier_candidates: Vec<MediaFormatItem> = all_video
        .iter()
        .filter(|f| fits_resolution_tier(f, tier_bounds))
        .cloned()
        .collect();
    if tier_candidates.is_empty() {
        tier_candidates = all_video;
    }

    let exact_candidates: Vec<MediaFormatItem> = tier_candidates
        .iter()
        .filter(|f| has_exact_filesize(f))
        .cloned()
        .collect();
    let mut pool = if !exact_candidates.is_empty() {
        exact_candidates
    } else {
        tier_candidates.clone()
    };

    pool.sort_by(|a, b| {
        let key_a = (
            a.width.unwrap_or(0).max(a.height.unwrap_or(0)),
            a.height.unwrap_or(0),
            u8::from(a.acodec.as_deref() != Some("none")),
            u8::from(a.ext.as_deref() == Some("mp4")),
            stream_bytes(a, duration),
        );
        let key_b = (
            b.width.unwrap_or(0).max(b.height.unwrap_or(0)),
            b.height.unwrap_or(0),
            u8::from(b.acodec.as_deref() != Some("none")),
            u8::from(b.ext.as_deref() == Some("mp4")),
            stream_bytes(b, duration),
        );
        key_b.cmp(&key_a)
    });

    let mut best_v = pool.into_iter().next()?;
    let mut v_id = best_v.format_id.clone().unwrap_or_default();
    let mut v_bytes = stream_bytes(&best_v, duration);

    let is_twitter_src = info
        .extractor
        .as_deref()
        .unwrap_or("")
        .to_lowercase()
        .contains("twitter")
        || is_twitter_status_url(if !url_hint.is_empty() {
            url_hint
        } else {
            info.webpage_url.as_deref().unwrap_or("")
        });

    if is_twitter_src
        && has_ffmpeg
        && v_bytes > 900 * 1024 * 1024
        && !best_v.protocol.as_deref().unwrap_or("").contains("m3u8")
    {
        let target_res = best_v.width.unwrap_or(0).max(best_v.height.unwrap_or(0));
        let mut hls_candidates: Vec<MediaFormatItem> = tier_candidates
            .iter()
            .filter(|f| {
                f.protocol.as_deref().unwrap_or("").contains("m3u8")
                    && f.width.unwrap_or(0).max(f.height.unwrap_or(0)) >= target_res
            })
            .cloned()
            .collect();
        if !hls_candidates.is_empty() {
            hls_candidates.sort_by(|a, b| {
                let res_cmp = b
                    .width
                    .unwrap_or(0)
                    .max(b.height.unwrap_or(0))
                    .cmp(&a.width.unwrap_or(0).max(a.height.unwrap_or(0)));
                if res_cmp != std::cmp::Ordering::Equal {
                    res_cmp
                } else {
                    b.tbr
                        .unwrap_or(0.0)
                        .partial_cmp(&a.tbr.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            });
            best_v = hls_candidates.into_iter().next().unwrap();
            v_id = best_v.format_id.clone().unwrap_or_default();
            v_bytes = stream_bytes(&best_v, duration);
        }
    }

    let mut is_exact = best_v.filesize.unwrap_or(0) > 0
        && !best_v.protocol.as_deref().unwrap_or("").contains("m3u8");
    let mut a_bytes: u64 = 0;
    let locked_id: String;

    if best_v.acodec.as_deref() == Some("none") && has_ffmpeg {
        let audio_only: Vec<MediaFormatItem> = formats
            .iter()
            .filter(|f| is_audio_only(f) && f.format_id.is_some())
            .cloned()
            .collect();
        let exact_audio: Vec<MediaFormatItem> = audio_only
            .iter()
            .filter(|f| has_exact_filesize(f))
            .cloned()
            .collect();
        let mut audio_pool = if !exact_audio.is_empty() {
            exact_audio
        } else {
            audio_only
        };

        if !audio_pool.is_empty() {
            audio_pool.sort_by(|a, b| {
                let ext_a = u8::from(matches!(a.ext.as_deref(), Some("m4a") | Some("mp4")));
                let ext_b = u8::from(matches!(b.ext.as_deref(), Some("m4a") | Some("mp4")));
                let c1 = ext_b.cmp(&ext_a);
                if c1 != std::cmp::Ordering::Equal {
                    return c1;
                }
                let br_a = a.abr.or(a.tbr).unwrap_or(0.0);
                let br_b = b.abr.or(b.tbr).unwrap_or(0.0);
                let c2 = br_b
                    .partial_cmp(&br_a)
                    .unwrap_or(std::cmp::Ordering::Equal);
                if c2 != std::cmp::Ordering::Equal {
                    return c2;
                }
                stream_bytes(b, duration).cmp(&stream_bytes(a, duration))
            });
            let best_a = audio_pool.into_iter().next().unwrap();
            let a_id = best_a.format_id.clone().unwrap_or_default();
            a_bytes = stream_bytes(&best_a, duration);
            locked_id = format!("{v_id}+{a_id}");
            is_exact = is_exact && best_a.filesize.unwrap_or(0) > 0;
        } else {
            locked_id = format!("{v_id}+bestaudio/{v_id}");
            is_exact = false;
        }
    } else {
        locked_id = v_id;
    }

    Some(SelectedStream {
        locked_id,
        best_v,
        total_bytes: v_bytes + a_bytes,
        is_exact,
    })
}

pub fn select_twitter_fallback_stream_url(
    info: &MediaInfo,
    selected_quality: VideoQuality,
    locked_id: Option<&str>,
    has_ffmpeg: bool,
) -> Option<String> {
    let formats = info.formats.as_deref()?;
    if formats.is_empty() {
        return None;
    }

    let hls_formats: Vec<&MediaFormatItem> = formats
        .iter()
        .filter(|f| f.protocol.as_deref().unwrap_or("").contains("m3u8") && f.url.is_some())
        .collect();
    let http_formats: Vec<&MediaFormatItem> = formats
        .iter()
        .filter(|f| !f.protocol.as_deref().unwrap_or("").contains("m3u8") && f.url.is_some())
        .collect();

    if let Some(lid) = locked_id {
        let base_locked = lid.split('+').next().unwrap_or(lid);
        for f in formats {
            if let Some(fid) = f.format_id.as_deref() {
                if (fid == lid || fid == base_locked) && f.url.is_some() {
                    let f_bytes = f.filesize.or(f.filesize_approx).unwrap_or(0);
                    if !f.protocol.as_deref().unwrap_or("").contains("m3u8")
                        && f_bytes > 900 * 1024 * 1024
                        && has_ffmpeg
                        && !hls_formats.is_empty()
                    {
                        return hls_formats[0].url.clone();
                    }
                    return f.url.clone();
                }
            }
        }
    }

    let tier_bounds = selected_quality.tier_bounds();
    let pool_base: Vec<&MediaFormatItem> = if !http_formats.is_empty() {
        http_formats
    } else {
        formats.iter().collect()
    };
    let mut candidates: Vec<&MediaFormatItem> = pool_base
        .iter()
        .copied()
        .filter(|f| fits_resolution_tier(f, tier_bounds))
        .collect();
    if candidates.is_empty() {
        candidates = pool_base;
    }
    candidates.sort_by(|a, b| {
        let ra = a.width.unwrap_or(0).max(a.height.unwrap_or(0));
        let rb = b.width.unwrap_or(0).max(b.height.unwrap_or(0));
        let c1 = rb.cmp(&ra);
        if c1 != std::cmp::Ordering::Equal {
            return c1;
        }
        let c2 = b
            .tbr
            .unwrap_or(0.0)
            .partial_cmp(&a.tbr.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal);
        if c2 != std::cmp::Ordering::Equal {
            return c2;
        }
        b.filesize.unwrap_or(0).cmp(&a.filesize.unwrap_or(0))
    });

    let chosen = candidates.into_iter().next()?;
    let chosen_bytes = chosen.filesize.or(chosen.filesize_approx).unwrap_or(0);
    if !chosen.protocol.as_deref().unwrap_or("").contains("m3u8")
        && chosen_bytes > 900 * 1024 * 1024
        && has_ffmpeg
        && !hls_formats.is_empty()
    {
        return hls_formats[0].url.clone();
    }
    chosen.url.clone()
}

pub fn extract_best_thumbnail_url(info: &MediaInfo) -> Option<String> {
    if let Some(suno_img) = &info.suno_img_url {
        return Some(suno_img.clone());
    }
    if let Some(thumbs) = &info.thumbnails {
        let valid: Vec<&ThumbnailItem> = thumbs
            .iter()
            .filter(|t| {
                let u = t.url.as_deref().unwrap_or("");
                (u.starts_with("http://") || u.starts_with("https://")) && !u.ends_with(".mhtml")
            })
            .collect();
        if !valid.is_empty() {
            let mut sized: Vec<&ThumbnailItem> = valid
                .iter()
                .copied()
                .filter(|t| t.width.unwrap_or(0) > 0 || t.height.unwrap_or(0) > 0)
                .collect();
            if !sized.is_empty() {
                sized.sort_by_key(|t| {
                    let w = t
                        .width
                        .unwrap_or_else(|| t.height.unwrap_or(135) * 16 / 9)
                        as i64;
                    (w - 240).abs()
                });
                return sized[0].url.clone();
            }
            return valid.last().and_then(|t| t.url.clone());
        }
    }
    if let Some(direct) = &info.thumbnail {
        if direct.starts_with("http://") || direct.starts_with("https://") {
            return Some(direct.clone());
        }
    }
    if info.is_playlist && !info.normalized_entries.is_empty() {
        let first = &info.normalized_entries[0];
        if let Some(t) = &first.thumbnail {
            if t.starts_with("http://") || t.starts_with("https://") {
                return Some(t.clone());
            }
        }
        if let Some(vid_id) = &first.id {
            if vid_id.len() == 11 {
                return Some(format!("https://i.ytimg.com/vi/{vid_id}/mqdefault.jpg"));
            }
        }
    }
    if let Some(vid_id) = &info.id {
        let ctx = format!(
            "{} {}",
            info.extractor.as_deref().unwrap_or(""),
            info.webpage_url.as_deref().unwrap_or("")
        )
        .to_lowercase();
        if vid_id.len() == 11 && ctx.contains("youtube") {
            return Some(format!("https://i.ytimg.com/vi/{vid_id}/mqdefault.jpg"));
        }
    }
    None
}

pub fn download_thumbnail_pixels(url: &str) -> Result<ThumbnailPixels, String> {
    let client = build_http_client(6)?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("Error descargando miniatura: {e}"))?;
    if !resp.status().is_success() {
        return Err("Miniatura no disponible".into());
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("Error leyendo bytes de miniatura: {e}"))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Miniatura demasiado grande".into());
    }

    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Error decodificando miniatura: {e}"))?;
    let resized = img.resize_to_fill(88, 50, FilterType::Triangle);
    let rgba = resized.to_rgba8();
    Ok(ThumbnailPixels {
        url: url.to_string(),
        width: 88,
        height: 50,
        rgba: rgba.into_raw(),
    })
}
