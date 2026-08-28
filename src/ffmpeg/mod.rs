mod detect;
mod remux;

pub use detect::detect_ffmpeg;
pub use remux::{remux_faststart, remux_to_mp4};
