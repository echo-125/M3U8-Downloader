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

/// 用 concat 协议合并自带 ftyp/moov 的分片。
///
/// 部分 fMP4 流没有独立的初始化段，分片各自携带 ftyp/moov，直接二进制拼接会
/// 重复出现多次初始化信息而无法播放，必须交给 ffmpeg 按 concat 协议重新组装。
pub async fn concat_copy_to_mp4(
    program: &str,
    concat_list: &Path,
    output: &Path,
) -> Result<PathBuf, FfmpegError> {
    if output.exists() {
        return Err(FfmpegError::Execution("输出文件已存在".into()));
    }
    let arguments = concat_arguments(concat_list, output);
    let output_result = run_command(program, &arguments, Some(FFMPEG_TIMEOUT)).await?;
    if output_result.status.success() && output.is_file() {
        return Ok(output.to_path_buf());
    }
    let _ = tokio::fs::remove_file(output).await;
    Err(FfmpegError::Conversion(last_error_lines(
        &output_result.stderr,
    )))
}

fn concat_arguments(concat_list: &Path, output: &Path) -> Vec<String> {
    vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "concat".to_string(),
        // 允许列表里出现绝对路径，否则 ffmpeg 会拒绝读取。
        "-safe".to_string(),
        "0".to_string(),
        "-i".to_string(),
        concat_list.to_string_lossy().into_owned(),
        "-c".to_string(),
        "copy".to_string(),
        "-y".to_string(),
        output.to_string_lossy().into_owned(),
    ]
}

/// 生成 ffmpeg concat 列表文件。
///
/// concat 语法中单引号内的反斜杠是转义字符，Windows 路径要统一换成正斜杠，
/// 路径里的单引号用 `\' ` 转义，否则会提前闭合导致 ffmpeg 解析失败。
pub fn write_concat_list(paths: &[PathBuf], list_path: &Path) -> Result<(), std::io::Error> {
    let mut content = String::new();
    for path in paths {
        let escaped = path
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "\\'");
        content.push_str("file '");
        content.push_str(&escaped);
        content.push_str("'\n");
    }
    std::fs::write(list_path, content)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_windows_paths_in_concat_list() {
        // concat 语法里单引号内的反斜杠是转义字符，Windows 路径必须换成正斜杠，
        // 路径里的单引号要转义，否则 ffmpeg 会提前闭合字符串导致解析失败。
        let paths = vec![
            PathBuf::from(r"C:\temp\segment 1.m4s"),
            PathBuf::from(r"C:\temp\it's.m4s"),
        ];
        let list_path =
            std::env::temp_dir().join(format!("cat-catch-concat-{}.txt", std::process::id()));
        write_concat_list(&paths, &list_path).unwrap();
        let content = std::fs::read_to_string(&list_path).unwrap();
        let _ = std::fs::remove_file(&list_path);
        assert_eq!(
            content,
            "file 'C:/temp/segment 1.m4s'\nfile 'C:/temp/it\\'s.m4s'\n"
        );
    }
}
