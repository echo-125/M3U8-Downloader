use std::path::Path;

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
        return Some("ffmpeg".to_string());
    }
    None
}

async fn verify_ffmpeg(program: &str) -> bool {
    if program != "ffmpeg" && !Path::new(program).is_file() {
        return false;
    }
    crate::ffmpeg::remux::run_version_check(program)
        .await
        .is_ok()
}
