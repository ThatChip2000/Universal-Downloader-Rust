#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod downloader;
mod engines;
mod extractors;
mod models;
mod theme;
mod ui;

use egui::IconData;
use std::sync::Arc;
use ui::UniversalDownloaderApp;

static APP_ICON_BYTES: &[u8] = include_bytes!("../assets/app_icon.ico");

fn load_window_icon() -> Option<Arc<IconData>> {
    let img = image::load_from_memory(APP_ICON_BYTES).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Arc::new(IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    }))
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 770.0])
        .with_min_inner_size([780.0, 650.0])
        .with_title("Universal Downloader - YouTube | TikTok | Instagram | Facebook | X");

    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Universal Downloader",
        options,
        Box::new(|cc| Ok(Box::new(UniversalDownloaderApp::new(cc)))),
    )
}
