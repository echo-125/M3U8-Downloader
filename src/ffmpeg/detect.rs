use std::path::{Path, PathBuf};

use crate::config::Settings;

pub async fn detect_ffmpeg(settings: &Settings) -> Option<String> {
    if !settings.ffmpeg.auto_detect && !settings.ffmpeg.manual_path.trim().is_empty() {
        let path = settings.ffmpeg.manual_path.trim();
        return verify_ffmpeg(path).await.then(|| path.to_string());
    }

    let manual_path = settings.ffmpeg.manual_path.trim();
    if !manual_path.is_empty() && verify_ffmpeg(manual_path).await {
        return Some(manual_path.to_string());
    }
    if settings.ffmpeg.auto_detect && verify_ffmpeg("ffmpeg").await {
        // 解析出 PATH 里的完整路径，设置页才能显示本地位置而不是裸命令名。
        return Some(find_in_path("ffmpeg").unwrap_or_else(|| "ffmpeg".to_string()));
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

async fn verify_ffmpeg(program: &str) -> bool {
    if program != "ffmpeg" && !Path::new(program).is_file() {
        return false;
    }
    crate::ffmpeg::remux::run_version_check(program)
        .await
        .is_ok()
}
