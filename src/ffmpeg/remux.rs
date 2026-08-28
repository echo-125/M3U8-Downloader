use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use thiserror::Error;
use tokio::process::Command;

const FFMPEG_TIMEOUT: Duration = Duration::from_secs(300);
const FFMPEG_VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("ffmpeg 执行超时")]
    Timeout,
    #[error("ffmpeg 执行失败：{0}")]
    Execution(String),
    #[error("ffmpeg 转换失败：{0}")]
    Conversion(String),
}

pub async fn run_version_check(program: &str) -> Result<(), FfmpegError> {
    let version_arguments = vec!["-version".to_string()];
    let output = run_command(program, &version_arguments, Some(FFMPEG_VERSION_TIMEOUT)).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(FfmpegError::Execution("无法获取版本信息".into()))
    }
}

/// fMP4 直接拼接的产物缺少索引，重封装一次并把 moov 前置，便于边下边播。
pub async fn remux_faststart(
    program: &str,
    input: &Path,
    output: &Path,
) -> Result<PathBuf, FfmpegError> {
    if output.exists() {
        return Err(FfmpegError::Execution("输出文件已存在".into()));
    }
    let arguments = copy_arguments(input, output, true);
    let output_result = run_command(program, &arguments, Some(FFMPEG_TIMEOUT)).await?;
    if output_result.status.success() && output.is_file() {
        return Ok(output.to_path_buf());
    }
    let _ = tokio::fs::remove_file(output).await;
    Err(FfmpegError::Conversion(last_error_lines(
        &output_result.stderr,
    )))
}

pub async fn remux_to_mp4(
    program: &str,
    input: &Path,
    output: &Path,
) -> Result<PathBuf, FfmpegError> {
    if output.exists() {
        return Err(FfmpegError::Execution("输出文件已存在".into()));
    }

    let copy_arguments = copy_arguments(input, output, false);
    let output_result = run_command(program, &copy_arguments, Some(FFMPEG_TIMEOUT)).await?;
    if output_result.status.success() && output.is_file() {
        return Ok(output.to_path_buf());
    }
    let _ = tokio::fs::remove_file(output).await;

    let encode_arguments = encode_arguments(input, output);
    let output_result = run_command(program, &encode_arguments, Some(FFMPEG_TIMEOUT)).await?;
    if output_result.status.success() && output.is_file() {
        return Ok(output.to_path_buf());
    }
    let _ = tokio::fs::remove_file(output).await;
    Err(FfmpegError::Conversion(last_error_lines(
        &output_result.stderr,
    )))
}

fn copy_arguments(input: &Path, output: &Path, faststart: bool) -> Vec<String> {
    build_arguments(input, output, &["-c", "copy"], faststart)
}

fn encode_arguments(input: &Path, output: &Path) -> Vec<String> {
    build_arguments(
        input,
        output,
        &["-c:v", "libx264", "-preset", "fast", "-c:a", "aac"],
        false,
    )
}

fn build_arguments(input: &Path, output: &Path, codec: &[&str], faststart: bool) -> Vec<String> {
    let mut arguments = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        input.to_string_lossy().into_owned(),
    ];
    arguments.extend(codec.iter().map(|value| value.to_string()));
    if faststart {
        arguments.push("-movflags".to_string());
        arguments.push("faststart".to_string());
    }
    arguments.push("-y".to_string());
    arguments.push(output.to_string_lossy().into_owned());
    arguments
}

async fn run_command(
    program: &str,
    arguments: &[String],
    timeout: Option<Duration>,
) -> Result<std::process::Output, FfmpegError> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 超时后等待 future 会被丢弃，必须让子进程随之结束，避免残留 ffmpeg 进程。
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }

    let child = command
        .spawn()
        .map_err(|_| FfmpegError::Execution("无法启动 ffmpeg".into()))?;
    let output = if let Some(timeout) = timeout {
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => {
                result.map_err(|_| FfmpegError::Execution("读取 ffmpeg 输出失败".into()))?
            }
            Err(_) => return Err(FfmpegError::Timeout),
        }
    } else {
        child
            .wait_with_output()
            .await
            .map_err(|_| FfmpegError::Execution("读取 ffmpeg 输出失败".into()))?
    };
    Ok(output)
}

fn last_error_lines(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}
