mod detect;
mod remux;

/// ffmpeg 检测结果：可执行文件完整路径 + 版本号。
/// 界面只在右下角状态栏和设置页展示版本号，完整路径放悬停提示里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegInfo {
    pub path: String,
    pub version: String,
}

pub use detect::detect_ffmpeg;
pub use remux::{concat_copy_to_mp4, remux_faststart, remux_to_mp4, write_concat_list};
