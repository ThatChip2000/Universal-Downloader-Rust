use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub fn get_appdata_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AppData")
            .join("Local")
    });
    let dir = base.join("UniversalDownloader");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn get_appdata_bin_dir() -> PathBuf {
    let bin = get_appdata_dir().join("bin");
    let _ = fs::create_dir_all(&bin);
    bin
}

pub fn get_config_path() -> PathBuf {
    get_appdata_dir().join("config.json")
}

pub fn default_downloads_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Downloads")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub theme: Option<String>,
    pub download_dir: Option<String>,
    pub ext_engine_mtime: Option<f64>,
    pub ext_engine_version: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

pub fn load_config() -> AppConfig {
    let path = get_config_path();
    if let Ok(raw) = fs::read_to_string(path) {
        if let Ok(cfg) = serde_json::from_str::<AppConfig>(&raw) {
            return cfg;
        }
    }
    AppConfig::default()
}

pub fn save_config(cfg: &AppConfig) {
    let path = get_config_path();
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, json);
    }
}
