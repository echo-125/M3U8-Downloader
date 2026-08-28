use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use crate::core::{
    error::CoreError,
    format::{detect_format, SegmentFormat},
};

/// 拼接时最多扫描多长的头部来定位 TS 同步字节。
const TS_SCAN_LIMIT: usize = 10 * 1024 * 1024;
const TS_PACKET_SIZE: usize = 188;
/// 连续多少个同步字节才认定为合法的 TS 起始位置。
const TS_SYNC_PACKETS: usize = 4;

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
            if !convert_to_mp4 {
                let output = unique_output_path(output_directory, output_name, "ts");
                tokio::fs::rename(&temporary_input, &output)
                    .await
                    .map_err(|_| CoreError::Io("保存 TS 输出失败".into()))?;
                return Ok(MergeResult {
                    output_path: output,
                    used_ffmpeg: false,
                    message: "TS 分片已直接合并".into(),
                });
            }
            let Some(program) = ffmpeg_program else {
                let _ = std::fs::remove_file(&temporary_input);
                return Err(CoreError::InvalidInput("未找到可用的 ffmpeg".into()));
            };
            let output = unique_output_path(output_directory, output_name, "mp4");
            match crate::ffmpeg::remux_to_mp4(program, &temporary_input, &output).await {
                Ok(_) => {
                    let _ = tokio::fs::remove_file(&temporary_input).await;
                    Ok(MergeResult {
                        output_path: output,
                        used_ffmpeg: true,
                        message: "TS 分片已转换为 MP4".into(),
                    })
                }
                Err(error) => {
                    // 流拷贝与重编码都失败时降级为直接输出 TS，避免整个任务失败。
                    let ts_output = unique_output_path(output_directory, output_name, "ts");
                    tokio::fs::rename(&temporary_input, &ts_output)
                        .await
                        .map_err(|_| CoreError::Io("保存 TS 输出失败".into()))?;
                    Ok(MergeResult {
                        output_path: ts_output,
                        used_ffmpeg: false,
                        message: format!("ffmpeg 转换失败（{error}），已直接输出 TS"),
                    })
                }
            }
        }
        SegmentFormat::Fmp4 => {
            let initialization = initialization
                .filter(|path| path.is_file())
                .ok_or_else(|| CoreError::InvalidSegment("fMP4 分片缺少初始化段".into()))?;
            let output = unique_output_path(output_directory, output_name, "mp4");
            let raw_output = output.with_extension("raw.mp4");
            concatenate(
                segment_paths,
                &raw_output,
                Some(initialization.to_path_buf()),
            )
            .await?;
            // 直接拼接的产物缺少 moov 索引，能用 ffmpeg 时重封装一次并前置索引。
            let Some(program) = ffmpeg_program else {
                tokio::fs::rename(&raw_output, &output)
                    .await
                    .map_err(|_| CoreError::Io("保存 fMP4 输出失败".into()))?;
                return Ok(MergeResult {
                    output_path: output,
                    used_ffmpeg: false,
                    message: "未检测到 ffmpeg，fMP4 分片已直接拼接".into(),
                });
            };
            if let Err(error) = crate::ffmpeg::remux_faststart(program, &raw_output, &output).await
            {
                let _ = tokio::fs::remove_file(&raw_output).await;
                return Err(CoreError::Ffmpeg(error.to_string()));
            }
            let _ = tokio::fs::remove_file(&raw_output).await;
            Ok(MergeResult {
                output_path: output,
                used_ffmpeg: true,
                message: "fMP4 分片已重封装为 MP4".into(),
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
            &left.file_name().unwrap_or_default().to_string_lossy(),
            &right.file_name().unwrap_or_default().to_string_lossy(),
        )
    });

    let mut result = MergeScanResult {
        ts_segments: Vec::new(),
        fmp4_segments: Vec::new(),
        initialization: None,
    };
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
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
        Some(initialization) => concatenate_fmp4(&paths, &initialization, &output),
        None => concatenate_files(&paths, &output),
    })
    .await
    .unwrap_or(Err(CoreError::Io("合并任务异常终止".into())))
}

/// TS 拼接：丢弃首部可能混入的垃圾数据，从第一个合法的同步字节开始写。
fn concatenate_files(paths: &[PathBuf], output: &Path) -> Result<(), CoreError> {
    let mut writer =
        BufWriter::new(File::create(output).map_err(|_| CoreError::Io("创建合并文件失败".into()))?);
    let mut head = Vec::new();
    let mut head_written = false;
    let mut buffer = vec![0_u8; 256 * 1024];
    for path in paths {
        let mut input = File::open(path).map_err(|_| CoreError::Io("打开分片失败".into()))?;
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|_| CoreError::Io("读取分片失败".into()))?;
            if read == 0 {
                break;
            }
            let chunk = &buffer[..read];
            if head_written {
                write_chunk(&mut writer, chunk)?;
                continue;
            }
            let take = (TS_SCAN_LIMIT - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
            if head.len() >= TS_SCAN_LIMIT {
                let offset = ts_sync_offset(&head);
                write_chunk(&mut writer, &head[offset..])?;
                head_written = true;
                write_chunk(&mut writer, &chunk[take..])?;
            }
        }
    }
    if !head_written {
        let offset = ts_sync_offset(&head);
        write_chunk(&mut writer, &head[offset..])?;
    }
    writer
        .flush()
        .map_err(|_| CoreError::Io("写入合并文件失败".into()))
}

/// fMP4 拼接：初始化段原样写入，分片需要跳过 styp box 后再写入。
fn concatenate_fmp4(
    paths: &[PathBuf],
    initialization: &Path,
    output: &Path,
) -> Result<(), CoreError> {
    let mut writer =
        BufWriter::new(File::create(output).map_err(|_| CoreError::Io("创建合并文件失败".into()))?);
    let mut init =
        File::open(initialization).map_err(|_| CoreError::Io("打开初始化段失败".into()))?;
    std::io::copy(&mut init, &mut writer).map_err(|_| CoreError::Io("写入初始化段失败".into()))?;
    for path in paths {
        let mut input = File::open(path).map_err(|_| CoreError::Io("打开分片失败".into()))?;
        copy_stripping_styp(&mut input, &mut writer)?;
    }
    writer
        .flush()
        .map_err(|_| CoreError::Io("写入合并文件失败".into()))
}

fn write_chunk(writer: &mut impl Write, chunk: &[u8]) -> Result<(), CoreError> {
    writer
        .write_all(chunk)
        .map_err(|_| CoreError::Io("写入合并文件失败".into()))
}

/// 找到第一个连续 4 个包都是同步字节的位置，找不到时返回 0。
fn ts_sync_offset(head: &[u8]) -> usize {
    let span = TS_PACKET_SIZE * (TS_SYNC_PACKETS - 1);
    if head.len() <= span {
        return 0;
    }
    (0..=head.len() - span - 1)
        .find(|&index| {
            (0..TS_SYNC_PACKETS).all(|packet| head[index + packet * TS_PACKET_SIZE] == 0x47)
        })
        .unwrap_or(0)
}

fn copy_stripping_styp(input: &mut File, output: &mut impl Write) -> Result<(), CoreError> {
    while let Some((header, body)) = read_box_header(input)? {
        let is_styp = header.len() >= 8 && &header[4..8] == b"styp";
        if is_styp && body != u64::MAX {
            skip_exact(input, body)?;
            continue;
        }
        write_chunk(output, &header)?;
        if body == u64::MAX {
            std::io::copy(input, output).map_err(|_| CoreError::Io("写入合并文件失败".into()))?;
            return Ok(());
        }
        copy_exact(input, output, body)?;
    }
    Ok(())
}

/// 读取一个 box 头部，返回头部字节与负载长度；负载为 u64::MAX 表示一直到文件末尾。
fn read_box_header(input: &mut File) -> Result<Option<(Vec<u8>, u64)>, CoreError> {
    let mut head = [0_u8; 8];
    let read = read_up_to(input, &mut head)?;
    if read == 0 {
        return Ok(None);
    }
    if read < 8 {
        return Ok(Some((head[..read].to_vec(), 0)));
    }
    let mut size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
    let mut header = head.to_vec();
    if size == 1 {
        let mut large = [0_u8; 8];
        if read_up_to(input, &mut large)? != 8 {
            return Ok(Some((header, 0)));
        }
        size = u64::from_be_bytes(large);
        header.extend_from_slice(&large);
    }
    if size == 0 {
        return Ok(Some((header, u64::MAX)));
    }
    Ok(Some((
        header.clone(),
        size.saturating_sub(header.len() as u64),
    )))
}

fn read_up_to(input: &mut File, buffer: &mut [u8]) -> Result<usize, CoreError> {
    let mut total = 0;
    while total < buffer.len() {
        match input.read(&mut buffer[total..]) {
            Ok(0) => break,
            Ok(read) => total += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(CoreError::Io("读取分片失败".into())),
        }
    }
    Ok(total)
}

fn copy_exact(input: &mut File, output: &mut impl Write, length: u64) -> Result<(), CoreError> {
    let mut limited = (&mut *input).take(length);
    std::io::copy(&mut limited, output).map_err(|_| CoreError::Io("写入合并文件失败".into()))?;
    Ok(())
}

fn skip_exact(input: &mut File, length: u64) -> Result<(), CoreError> {
    let mut remaining = length;
    let mut buffer = [0_u8; 8 * 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = read_up_to(input, &mut buffer[..want])?;
        if read == 0 {
            break;
        }
        remaining -= read as u64;
    }
    Ok(())
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

    #[test]
    fn trims_garbage_before_ts_sync() {
        let mut head = vec![0x00; 1000 + TS_SYNC_PACKETS * TS_PACKET_SIZE];
        for packet in 0..TS_SYNC_PACKETS {
            head[1000 + packet * TS_PACKET_SIZE] = 0x47;
        }
        assert_eq!(ts_sync_offset(&head), 1000);
        // 数据太短或找不到同步字节时不裁剪
        assert_eq!(ts_sync_offset(&[0x47_u8; 4]), 0);
        assert_eq!(ts_sync_offset(&[0x00_u8; 1024]), 0);
    }

    #[test]
    fn strips_styp_boxes_when_concatenating_fmp4() {
        let mut input = Vec::new();
        input.extend_from_slice(&16_u32.to_be_bytes());
        input.extend_from_slice(b"styp");
        input.extend_from_slice(&[0_u8; 8]);
        input.extend_from_slice(&16_u32.to_be_bytes());
        input.extend_from_slice(b"moof");
        input.extend_from_slice(&[7_u8; 8]);

        let directory = temp_directory("merge");
        let source = directory.join("segment.m4s");
        std::fs::write(&source, &input).unwrap();
        let mut output = Vec::new();
        copy_stripping_styp(&mut File::open(&source).unwrap(), &mut output).unwrap();
        assert_eq!(output, input[16..]);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn temp_directory(prefix: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cat-catch-{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }
}
