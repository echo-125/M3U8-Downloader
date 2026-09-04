use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::id,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::core::{
    error::CoreError,
    format::{detect_format, SegmentFormat},
};

/// 合并中间文件的唯一计数器：进程内递增，保证并发合并各拿各的后缀。
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
            // 中间文件用进程内唯一的随机名，避免同名任务并发合并时
            // 因 unique_output_path「先检查再使用」非原子而互相覆盖。
            let temporary_input = unique_temporary_path(output_directory, "merge", "ts");
            // 合并是大块同步 IO，必须放到阻塞线程池，否则会卡住整个任务管理循环。
            if let Err(error) = concatenate(segment_paths, &temporary_input, None).await {
                // 拼接失败时清掉临时中间文件，避免残留 `.cat-catch-merge-*.ts`。
                let _ = tokio::fs::remove_file(&temporary_input).await;
                return Err(error);
            }
            if !convert_to_mp4 {
                let output = unique_output_path(output_directory, output_name, "ts");
                rename_cleaning_source(&temporary_input, &output).await?;
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
                    // `output` 是 ffmpeg 失败路径上的空占位文件，ffmpeg 没写入它就失败退出，
                    // 留着会污染输出目录、被手动合并扫描当成空分片，先清掉。
                    let _ = tokio::fs::remove_file(&output).await;
                    let ts_output = unique_output_path(output_directory, output_name, "ts");
                    rename_cleaning_source(&temporary_input, &ts_output).await?;
                    Ok(MergeResult {
                        output_path: ts_output,
                        used_ffmpeg: false,
                        message: format!("ffmpeg 转换失败（{error}），已直接输出 TS"),
                    })
                }
            }
        }
        SegmentFormat::Fmp4 => {
            let Some(initialization) = initialization.filter(|path| path.is_file()) else {
                // 没有独立初始化段时分片自身携带 ftyp/moov，二进制拼接会重复初始化信息，
                // 只能交给 ffmpeg 按 concat 协议重新组装。
                return concat_fmp4_segments(
                    segment_paths,
                    output_directory,
                    output_name,
                    ffmpeg_program,
                )
                .await;
            };
            let output = unique_output_path(output_directory, output_name, "mp4");
            // raw 中间文件同样用唯一名，避免并发同名任务撞 raw.mp4。
            let raw_output = unique_temporary_path(output_directory, "merge-raw", "mp4");
            if let Err(error) = concatenate(
                segment_paths,
                &raw_output,
                Some(initialization.to_path_buf()),
            )
            .await
            {
                // 拼接失败时 raw 与 output 占位都在磁盘上，清理掉避免污染输出目录。
                let _ = tokio::fs::remove_file(&raw_output).await;
                let _ = tokio::fs::remove_file(&output).await;
                return Err(error);
            }
            // 直接拼接的产物缺少 moov 索引，能用 ffmpeg 时重封装一次并前置索引。
            let Some(program) = ffmpeg_program else {
                rename_cleaning_source(&raw_output, &output).await?;
                return Ok(MergeResult {
                    output_path: output,
                    used_ffmpeg: false,
                    message: "未检测到 ffmpeg，fMP4 分片已直接拼接".into(),
                });
            };
            if let Err(error) = crate::ffmpeg::remux_faststart(program, &raw_output, &output).await
            {
                let _ = tokio::fs::remove_file(&raw_output).await;
                // ffmpeg 失败时 `output` 是空占位文件，没被真正写入，留着会污染输出目录。
                let _ = tokio::fs::remove_file(&output).await;
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

/// fMP4 分片缺少独立初始化段时的合并路径：交给 ffmpeg 的 concat 协议组装。
async fn concat_fmp4_segments(
    segment_paths: &[PathBuf],
    output_directory: &Path,
    output_name: &str,
    ffmpeg_program: Option<&str>,
) -> Result<MergeResult, CoreError> {
    let Some(program) = ffmpeg_program else {
        return Err(CoreError::InvalidInput(
            "fMP4 分片缺少初始化段，需要 ffmpeg 才能按 concat 协议合并".into(),
        ));
    };
    let output = unique_output_path(output_directory, output_name, "mp4");
    // 列表只是中间产物，用唯一名避免并发同名任务撞同一个列表文件。
    let concat_list = unique_temporary_path(output_directory, "concat", "txt");
    let paths = segment_paths.to_vec();
    let list_path = concat_list.clone();
    // spawn_blocking 的 JoinError 与 write_concat_list 的 io::Error 都归到同一个错误分支：
    // 只有在这里统一处理，才能保证任何失败路径都清理掉已占位的 `output` 与 concat 列表。
    let join_result =
        tokio::task::spawn_blocking(move || crate::ffmpeg::write_concat_list(&paths, &list_path))
            .await;
    let write_result = match join_result {
        Ok(result) => result,
        Err(_) => {
            // 闭包 panic（JoinError）：output 占位与列表都还没被写入，清理掉再返回。
            let _ = tokio::fs::remove_file(&output).await;
            let _ = tokio::fs::remove_file(&concat_list).await;
            return Err(CoreError::Io("写入 concat 列表任务异常".into()));
        }
    };
    if let Err(error) = write_result.map_err(|_| CoreError::Io("写入 concat 列表失败".into()))
    {
        // 还没开始合并，`output` 是空占位文件，清理掉。
        let _ = tokio::fs::remove_file(&output).await;
        let _ = tokio::fs::remove_file(&concat_list).await;
        return Err(error);
    }

    let result = crate::ffmpeg::concat_copy_to_mp4(program, &concat_list, &output).await;
    // 列表是中间产物，无论成败都要清理。
    let _ = tokio::fs::remove_file(&concat_list).await;
    match result {
        Ok(_) => Ok(MergeResult {
            output_path: output,
            used_ffmpeg: true,
            message: "fMP4 分片已通过 concat 协议合并".into(),
        }),
        Err(error) => {
            // ffmpeg 失败时 `output` 是空占位文件，没真正写入；留着会污染输出目录。
            let _ = tokio::fs::remove_file(&output).await;
            Err(CoreError::Ffmpeg(error.to_string()))
        }
    }
}

/// 合并中间文件的唯一路径：进程内原子递增计数 + 纳秒时间戳 + 进程 id，
/// 三者组合保证并发合并时各拿各的中间文件名，不会互相覆盖。
/// 不用 `unique_output_path` 的 `while exists` 循环——它「先检查再使用」非原子，
/// 两个并发任务会同时看到同名文件不存在，各自创建同一个中间文件互相覆盖，
/// 最终各自 rename 成不同输出名但内容相同（即「文件名不同、内容一样」的现象）。
fn unique_temporary_path(directory: &Path, prefix: &str, extension: &str) -> PathBuf {
    let counter = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    directory.join(format!(
        ".cat-catch-{prefix}-{nanos}-{counter}-{pid}.{extension}",
        pid = id()
    ))
}

/// 把中间文件改名为最终输出；rename 失败时清理中间文件再返回错误。
///
/// rename 失败（目标被占用、权限被拒，Windows 上播放器锁文件常见）时，source
/// 会残留在输出目录。它带 `.cat-catch-` 隐藏前缀且扩展名是 ts/mp4，会被手动合并
/// 扫描当成正常分片混进输出，必须清掉。
async fn rename_cleaning_source(source: &Path, destination: &Path) -> Result<(), CoreError> {
    match tokio::fs::rename(source, destination).await {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = tokio::fs::remove_file(source).await;
            Err(CoreError::Io("保存合并输出失败".into()))
        }
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

/// 生成唯一的最终输出路径：避免已有文件被覆盖，也避免并发同名任务撞同一文件名。
///
/// 不能用 `while path.exists()` 循环——它「先检查再使用」非原子，两个并发任务会同时
/// 看到同名文件不存在、各自拿到同一个路径，先完成的 rename 后，后完成的再 rename 覆盖，
/// 于是两个任务最终指向同一个文件、内容是后完成那份（即「文件名相同、内容相同」的现象）。
/// 这里用 `create_new` 原子占位：谁先创建成功谁独占这个名字并保留占位文件；并发对手
/// 拿到递增编号的名字。占位文件由调用方的 rename 原子覆盖，或出错时被清理。
pub fn unique_output_path(directory: &Path, name: &str, extension: &str) -> PathBuf {
    let sanitized = sanitize_filename(name);
    // 原名优先：单任务或无并发时拿到干净的名字。
    let primary = directory.join(format!("{sanitized}.{extension}"));
    if try_reserve(&primary).is_ok() {
        return primary;
    }
    // 原名已被占（已有文件或并发对手抢先占位），用计数器分配唯一编号。
    let mut index = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    loop {
        index += 1;
        let path = directory.join(format!("{sanitized} ({index}).{extension}"));
        if try_reserve(&path).is_ok() {
            return path;
        }
        // 极端情况（编号也被占）继续递增，计数器已在前一步分配，这里用本地变量循环。
    }
}

/// 用 create_new 原子占位：成功返回 Ok，文件已存在返回 Err。
/// 占位文件保留在磁盘上，由调用方的 rename 覆盖，避免删除后产生新的竞态窗口。
fn try_reserve(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map(|_| ())
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
