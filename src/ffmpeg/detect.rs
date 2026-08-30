use std::path::{Path, PathBuf};

use crate::config::Settings;
use crate::ffmpeg::FfmpegInfo;

/// 检测可用 ffmpeg，返回其路径与版本号；找不到返回 None。
pub async fn detect_ffmpeg(settings: &Settings) -> Option<FfmpegInfo> {
    if !settings.ffmpeg.auto_detect && !settings.ffmpeg.manual_path.trim().is_empty() {
        let path = settings.ffmpeg.manual_path.trim();
        return verify_ffmpeg(path).await.map(|version| FfmpegInfo {
            path: path.to_string(),
            version,
        });
    }

    let manual_path = settings.ffmpeg.manual_path.trim();
    if !manual_path.is_empty() {
        if let Some(version) = verify_ffmpeg(manual_path).await {
            return Some(FfmpegInfo {
                path: manual_path.to_string(),
                version,
            });
        }
    }
    if settings.ffmpeg.auto_detect {
        if let Some(version) = verify_ffmpeg("ffmpeg").await {
            // 解析出 PATH 里的完整路径，设置页才能显示本地位置而不是裸命令名。
            let path = find_in_path("ffmpeg").unwrap_or_else(|| "ffmpeg".to_string());
            return Some(FfmpegInfo { path, version });
        }
    }
    None
}

/// 在 PATH 中查找可执行文件的完整路径，Windows 上尝试常见扩展名。
fn find_in_path(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for directory in std::env::split_paths(&path_var) {
        if cfg!(windows) {
            for extension in ["exe", "bat", "cmd"] {
                let mut candidate = directory.join(program);
                candidate.set_extension(extension);
                candidates.push(candidate);
            }
        } else {
            candidates.push(directory.join(program));
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
}

async fn verify_ffmpeg(program: &str) -> Option<String> {
    if program != "ffmpeg" && !Path::new(program).is_file() {
        return None;
    }
    crate::ffmpeg::remux::run_version(program).await.ok()
}
