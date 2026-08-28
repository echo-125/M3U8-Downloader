use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::core::{
    error::CoreError,
    format::{detect_format, SegmentFormat},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeScanResult {
    pub ts_segments: Vec<PathBuf>,
    pub fmp4_segments: Vec<PathBuf>,
    pub initialization: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
    pub output_path: PathBuf,
    pub used_ffmpeg: bool,
    pub message: String,
}

pub async fn merge_segments(
    segment_paths: &[PathBuf],
    initialization: Option<&Path>,
    output_directory: &Path,
    output_name: &str,
    convert_to_mp4: bool,
    ffmpeg_program: Option<&str>,
) -> Result<MergeResult, CoreError> {
    if segment_paths.is_empty() {
        return Err(CoreError::InvalidInput("没有可合并的分片".into()));
    }
    tokio::fs::create_dir_all(output_directory)
        .await
        .map_err(|_| CoreError::Io("创建输出目录失败".into()))?;

    let format = inspect_segment_format(&segment_paths[0])?;
    if segment_paths
        .iter()
        .any(|path| inspect_segment_format(path).ok() != Some(format))
    {
        return Err(CoreError::InvalidSegment("分片格式不一致".into()));
    }

    match format {
        SegmentFormat::Ts => {
            // 临时文件使用唯一名称，避免同名任务并发合并时互相覆盖。
            let temporary_input =
                unique_output_path(output_directory, &format!("{output_name}.temporary"), "ts");
            // 合并是大块同步 IO，必须放到阻塞线程池，否则会卡住整个任务管理循环。
            concatenate(segment_paths, &temporary_input, None).await?;
            if convert_to_mp4 {
                let Some(program) = ffmpeg_program else {
                    let _ = std::fs::remove_file(&temporary_input);
                    return Err(CoreError::InvalidInput("未找到可用的 ffmpeg".into()));
                };
                let output = unique_output_path(output_directory, output_name, "mp4");
                if let Err(error) =
                    crate::ffmpeg::remux_to_mp4(program, &temporary_input, &output).await
                {
                    let _ = tokio::fs::remove_file(&temporary_input).await;
                    return Err(CoreError::Ffmpeg(error.to_string()));
                }
                let _ = tokio::fs::remove_file(&temporary_input).await;
                Ok(MergeResult {
                    output_path: output,
                    used_ffmpeg: true,
                    message: "TS 分片已转换为 MP4".into(),
                })
            } else {
                let output = unique_output_path(output_directory, output_name, "ts");
                tokio::fs::rename(&temporary_input, &output)
                    .await
                    .map_err(|_| CoreError::Io("保存 TS 输出失败".into()))?;
                Ok(MergeResult {
                    output_path: output,
                    used_ffmpeg: false,
                    message: "TS 分片已直接合并".into(),
                })
            }
        }
        SegmentFormat::Fmp4 => {
            let initialization = initialization
                .filter(|path| path.is_file())
                .ok_or_else(|| CoreError::InvalidSegment("fMP4 分片缺少初始化段".into()))?;
            let output = unique_output_path(output_directory, output_name, "mp4");
            concatenate(segment_paths, &output, Some(initialization.to_path_buf())).await?;
            Ok(MergeResult {
                output_path: output,
                used_ffmpeg: false,
                message: "fMP4 分片已直接拼接".into(),
            })
        }
        format => Err(CoreError::InvalidSegment(format!(
            "暂不支持合并{}分片",
            format.label()
        ))),
    }
}

pub async fn scan_merge_folder(folder: &Path) -> Result<MergeScanResult, CoreError> {
    let mut entries = tokio::fs::read_dir(folder)
        .await
        .map_err(|_| CoreError::Io("读取合并文件夹失败".into()))?;
    let mut paths = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| CoreError::Io("读取合并文件夹失败".into()))?
    {
        let path = entry.path();
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort_by(|left, right| {
        natural_compare(
            &left.file_name().unwrap().to_string_lossy(),
            &right.file_name().unwrap().to_string_lossy(),
        )
    });

    let mut result = MergeScanResult {
        ts_segments: Vec::new(),
        fmp4_segments: Vec::new(),
        initialization: None,
    };
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case("init.mp4") || name.eq_ignore_ascii_case("init.m4s") {
            result.initialization = Some(path);
            continue;
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if !matches!(
            extension.to_ascii_lowercase().as_str(),
            "ts" | "m4s" | "mp4"
        ) {
            continue;
        }
        match inspect_segment_format(&path) {
            Ok(SegmentFormat::Ts) => result.ts_segments.push(path),
            Ok(SegmentFormat::Fmp4) => result.fmp4_segments.push(path),
            _ => {}
        }
    }
    Ok(result)
}

pub fn safe_remove_directory(directory: &Path) -> Result<(), CoreError> {
    if !directory.is_dir() {
        return Ok(());
    }
    std::fs::remove_dir_all(directory).map_err(|_| CoreError::Io("清理临时目录失败".into()))
}

pub fn unique_output_path(directory: &Path, name: &str, extension: &str) -> PathBuf {
    let sanitized = sanitize_filename(name);
    let mut path = directory.join(format!("{sanitized}.{extension}"));
    let mut index = 1;
    while path.exists() {
        path = directory.join(format!("{sanitized} ({index}).{extension}"));
        index += 1;
    }
    path
}

pub fn sanitize_filename(name: &str) -> String {
    let mut result = String::new();
    for character in name.chars() {
        if matches!(
            character,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        ) {
            result.push('_');
        } else if character.is_control() {
            continue;
        } else {
            result.push(character);
        }
    }
    let result = result.trim().trim_matches('.').to_string();
    if result.is_empty() {
        "video".to_string()
    } else {
        result
    }
}

fn inspect_segment_format(path: &Path) -> Result<SegmentFormat, CoreError> {
    let mut file = File::open(path).map_err(|_| CoreError::Io("读取分片失败".into()))?;
    let mut header = [0_u8; 16];
    let read = file
        .read(&mut header)
        .map_err(|_| CoreError::Io("读取分片头部失败".into()))?;
    Ok(detect_format(&header[..read]))
}

/// 在阻塞线程池中拼接分片，避免大文件合并卡住异步任务管理循环。
async fn concatenate(
    paths: &[PathBuf],
    output: &Path,
    initialization: Option<PathBuf>,
) -> Result<(), CoreError> {
    let paths = paths.to_vec();
    let output = output.to_path_buf();
    tokio::task::spawn_blocking(move || match initialization {
        Some(initialization) => concatenate_files_with_init(&paths, &initialization, &output),
        None => concatenate_files(&paths, &output),
    })
    .await
    .unwrap_or(Err(CoreError::Io("合并任务异常终止".into())))
}

fn concatenate_files(paths: &[PathBuf], output: &Path) -> Result<(), CoreError> {
    let mut output_file =
        File::create(output).map_err(|_| CoreError::Io("创建合并文件失败".into()))?;
    for path in paths {
        let mut input = File::open(path).map_err(|_| CoreError::Io("打开分片失败".into()))?;
        std::io::copy(&mut input, &mut output_file)
            .map_err(|_| CoreError::Io("写入合并文件失败".into()))?;
    }
    output_file
        .flush()
        .map_err(|_| CoreError::Io("写入合并文件失败".into()))
}

fn concatenate_files_with_init(
    paths: &[PathBuf],
    initialization: &Path,
    output: &Path,
) -> Result<(), CoreError> {
    let mut output_file =
        File::create(output).map_err(|_| CoreError::Io("创建合并文件失败".into()))?;
    let mut init =
        File::open(initialization).map_err(|_| CoreError::Io("打开初始化段失败".into()))?;
    std::io::copy(&mut init, &mut output_file)
        .map_err(|_| CoreError::Io("写入初始化段失败".into()))?;
    for path in paths {
        let mut input = File::open(path).map_err(|_| CoreError::Io("打开分片失败".into()))?;
        std::io::copy(&mut input, &mut output_file)
            .map_err(|_| CoreError::Io("写入合并文件失败".into()))?;
    }
    output_file
        .flush()
        .map_err(|_| CoreError::Io("写入合并文件失败".into()))
}

fn natural_compare(left: &str, right: &str) -> std::cmp::Ordering {
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(left_character), Some(right_character)) => {
                if left_character.is_ascii_digit() && right_character.is_ascii_digit() {
                    let left_number = take_number(&mut left);
                    let right_number = take_number(&mut right);
                    match left_number.cmp(&right_number) {
                        std::cmp::Ordering::Equal => continue,
                        ordering => return ordering,
                    }
                }
                match left_character.cmp(right_character) {
                    std::cmp::Ordering::Equal => {
                        left.next();
                        right.next();
                    }
                    ordering => return ordering,
                }
            }
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (None, None) => return std::cmp::Ordering::Equal,
        }
    }
}

fn take_number<I>(iterator: &mut std::iter::Peekable<I>) -> u64
where
    I: Iterator<Item = char>,
{
    let mut text = String::new();
    while let Some(character) = iterator.peek() {
        if character.is_ascii_digit() {
            text.push(*character);
            iterator.next();
        } else {
            break;
        }
    }
    text.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_windows_filename() {
        assert_eq!(sanitize_filename("a/b:c*?"), "a_b_c__");
        assert_eq!(sanitize_filename("  .  "), "video");
    }

    #[test]
    fn natural_compare_orders_numbers() {
        let mut names = vec!["seg10.ts", "seg2.ts"];
        names.sort_by(|left, right| natural_compare(left, right));
        assert_eq!(names, vec!["seg2.ts", "seg10.ts"]);
    }
}
