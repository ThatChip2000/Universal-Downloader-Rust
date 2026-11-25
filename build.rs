use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

fn compress_asset_if_needed(src: &Path, dst: &Path) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Falta el asset requerido: {}", src.display()),
        ));
    }

    let needs_rebuild = match (fs::metadata(src), fs::metadata(dst)) {
        (Ok(s_meta), Ok(d_meta)) => match (s_meta.modified(), d_meta.modified()) {
            (Ok(s_time), Ok(d_time)) => s_time > d_time || d_meta.len() == 0,
            _ => true,
        },
        _ => true,
    };

    if !needs_rebuild {
        return Ok(());
    }

    let mut input = File::open(src)?;
    let output = File::create(dst)?;
    let mut encoder = ZlibEncoder::new(output, Compression::default());
    io::copy(&mut input, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let assets_dir = manifest_dir.join("assets");

    for bin_name in ["yt-dlp.exe", "ffmpeg.exe", "qjs.exe"] {
        let src = assets_dir.join(bin_name);
        let dst = out_dir.join(format!("{bin_name}.zlib"));
        if let Err(err) = compress_asset_if_needed(&src, &dst) {
            panic!("Error comprimiendo {bin_name}: {err}");
        }
    }

    let icon_path = assets_dir.join("app_icon.ico");
    println!("cargo:rerun-if-changed={}", icon_path.display());

    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        if icon_path.exists() {
            res.set_icon(icon_path.to_str().expect("ruta de icono UTF-8"));
        }
        res.set("ProductName", "Universal Downloader (Rust)");
        res.set(
            "FileDescription",
            "Universal Downloader - Gestor de Descargas Multimedia (Rust)",
        );
        res.set("CompanyName", "Universal Media Tools");
        res.set("LegalCopyright", "Copyright (C) 2026 Universal Media Tools");
        res.set("FileVersion", "4.0.0.0");
        res.set("ProductVersion", "4.0.0.0");
        res.set("OriginalFilename", "Universal Downloader (Rust).exe");
        res.set("InternalName", "UniversalDownloaderRust");
        if let Err(err) = res.compile() {
            panic!("Error compilando recursos Windows: {err}");
        }
    }
}
