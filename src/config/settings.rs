use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("配置值无效：{0}")]
    Validation(String),
    #[error("配置文件保存失败")]
    Save,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyScheme {
    Http,
    Https,
    Socks5,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            scheme: ProxyScheme::Http,
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfmpegConfig {
    pub auto_detect: bool,
    pub manual_path: String,
}

impl Default for FfmpegConfig {
    fn default() -> Self {
        Self {
            auto_detect: true,
            manual_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub file_enabled: bool,
    pub max_gui_entries: usize,
    pub rotation: String,
    pub max_size_mb: u64,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file_enabled: true,
            max_gui_entries: 500,
            rotation: "daily".to_string(),
            max_size_mb: 10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AppearanceConfig {
    pub theme: ThemeKind,
    pub window_width: f32,
    pub window_height: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Light,
            window_width: 960.0,
            window_height: 720.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    Light,
    Dark,
}

impl ThemeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "亮色",
            Self::Dark => "暗色",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub download_path: String,
    pub max_concurrent_downloads: usize,
    pub max_workers: usize,
    pub auto_cleanup: bool,
    pub keep_temp: bool,
    pub tail_threshold: u8,
    pub tail_boost: u8,
    pub proxy: ProxyConfig,
    pub ffmpeg: FfmpegConfig,
    pub logging: LoggingConfig,
    pub appearance: AppearanceConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_path: default_download_path(),
            max_concurrent_downloads: 3,
            max_workers: 16,
            auto_cleanup: true,
            keep_temp: false,
            tail_threshold: 90,
            tail_boost: 2,
            proxy: ProxyConfig::default(),
            ffmpeg: FfmpegConfig::default(),
            logging: LoggingConfig::default(),
            appearance: AppearanceConfig::default(),
        }
    }
}

pub fn default_config_path() -> PathBuf {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));

    if let Some(directory) = executable_directory {
        let path = directory.join("config.json");
        if directory_is_writable(&directory) {
            return path;
        }
    }

    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cat-catch-assistant")
        .join("config.json")
}

fn default_download_path() -> String {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .to_string_lossy()
        .into_owned()
}

fn directory_is_writable(directory: &Path) -> bool {
    let probe = directory.join(".cat-catch-write-test");
    let writable = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&probe)
        .is_ok();
    let _ = fs::remove_file(probe);
    writable
}

impl Settings {
    pub fn load_or_default(path: Option<&Path>) -> (Self, Option<String>) {
        let owned_path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(default_config_path);
        let path = owned_path.as_path();
        if !path.exists() {
            let settings = Self::default();
            let warning = settings
                .save(Some(path))
                .err()
                .map(|_| "配置文件创建失败，本次使用默认设置".to_string());
            return (settings, warning);
        }

        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<Self>(&content) {
                Ok(mut settings) => match settings.validate() {
                    Ok(()) => (settings, None),
                    Err(error) => (
                        Self::default(),
                        Some(format!("配置无效，已使用默认设置：{error}")),
                    ),
                },
                Err(_) => (
                    Self::default(),
                    Some("配置文件格式无效，已使用默认设置".to_string()),
                ),
            },
            Err(_) => (
                Self::default(),
                Some("配置文件读取失败，已使用默认设置".to_string()),
            ),
        }
    }

    pub fn save(&self, path: Option<&Path>) -> Result<(), ConfigError> {
        let owned_path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(default_config_path);
        let path = owned_path.as_path();
        let mut normalized = self.clone();
        normalized.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| ConfigError::Save)?;
        }
        // 写入规范化后的配置：下载路径为空时补默认值，代理主机与 ffmpeg 路径去掉首尾空格。
        let content = serde_json::to_string_pretty(&normalized).map_err(|_| ConfigError::Save)?;
        fs::write(path, content).map_err(|_| ConfigError::Save)
    }

    pub fn validate(&mut self) -> Result<(), ConfigError> {
        if self.download_path.trim().is_empty() {
            self.download_path = default_download_path();
        }
        if !(1..=16).contains(&self.max_concurrent_downloads) {
            return Err(ConfigError::Validation(
                "最大并发任务数必须在 1 到 16 之间".into(),
            ));
        }
        if !(1..=64).contains(&self.max_workers) {
            return Err(ConfigError::Validation(
                "单任务线程数必须在 1 到 64 之间".into(),
            ));
        }
        if !(1..=99).contains(&self.tail_threshold) {
            return Err(ConfigError::Validation(
                "尾部加速阈值必须在 1% 到 99% 之间".into(),
            ));
        }
        if !(1..=8).contains(&self.tail_boost) {
            return Err(ConfigError::Validation(
                "尾部加速倍数必须在 1 到 8 之间".into(),
            ));
        }
        if self.proxy.enabled {
            if self.proxy.host.trim().is_empty() {
                return Err(ConfigError::Validation("启用代理时主机不能为空".into()));
            }
            if self.proxy.port == 0 {
                return Err(ConfigError::Validation("启用代理时端口必须有效".into()));
            }
        }
        // GUI 日志容量固定，界面不可编辑，手工改成其他值时纠正而不是丢弃整份配置。
        if self.logging.max_gui_entries != crate::logging::MAX_GUI_ENTRIES {
            self.logging.max_gui_entries = crate::logging::MAX_GUI_ENTRIES;
        }
        if self.logging.max_size_mb == 0 || self.logging.max_size_mb > 100 {
            return Err(ConfigError::Validation(
                "日志单文件大小必须在 1MB 到 100MB 之间".into(),
            ));
        }
        if !matches!(self.logging.rotation.as_str(), "daily" | "size") {
            return Err(ConfigError::Validation(
                "日志滚动策略仅支持 daily 或 size".into(),
            ));
        }
        if self.appearance.window_width < 820.0
            || self.appearance.window_height < 560.0
            || self.appearance.window_width > 4096.0
            || self.appearance.window_height > 4096.0
        {
            return Err(ConfigError::Validation("窗口尺寸超出支持范围".into()));
        }
        self.proxy.host = self.proxy.host.trim().to_string();
        self.ffmpeg.manual_path = self.ffmpeg.manual_path.trim().to_string();
        Ok(())
    }

    pub fn normalized_download_path(&self) -> PathBuf {
        PathBuf::from(self.download_path.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_invalid_ranges() {
        let mut settings = Settings {
            max_workers: 0,
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn validation_rejects_proxy_without_port() {
        let mut settings = Settings::default();
        settings.proxy.enabled = true;
        settings.proxy.host = "127.0.0.1".into();
        settings.proxy.port = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn save_and_load_round_trip() {
        let directory = tempfile_directory();
        let path = directory.join("config.json");
        let mut settings = Settings {
            max_workers: 8,
            ..Default::default()
        };
        settings.validate().unwrap();
        settings.save(Some(&path)).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let loaded: Settings = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.max_workers, 8);
        fs::remove_dir_all(directory).unwrap();
    }

    fn tempfile_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cat-catch-config-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }
}
