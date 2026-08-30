use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::{future::ready, stream, StreamExt, TryStreamExt};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc::UnboundedSender, OwnedSemaphorePermit, Semaphore},
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::Settings,
    core::{
        decrypt::{decrypt_aes128_cbc, implicit_iv},
        error::CoreError,
        events::{CoreLogLevel, TaskEvent, TaskSnapshot, TaskStatus},
        fetcher::PlaylistFetcher,
        format::{detect_format, diagnostic_message, SegmentFormat},
        headers::validate_headers,
        merge::{merge_segments, safe_remove_directory},
        playlist::{ByteRange, MediaSegment},
        retry::{parse_retry_after, MAX_AUTO_RETRIES},
        task::TaskManifest,
    },
};

pub struct DownloadTask {
    pub manifest: TaskManifest,
    pub settings: Settings,
    pub event_sender: UnboundedSender<TaskEvent>,
    pub cancellation_token: CancellationToken,
    pub global_permits: Arc<Semaphore>,
}

/// 超过该体积且不需要解密的分片改为流式写盘，避免整片驻留内存。
const STREAM_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024;
/// 用于格式识别的头部采样长度。
const HEADER_SAMPLE_BYTES: usize = 16;

#[derive(Debug)]
struct DownloadProgress {
    completed: usize,
    downloaded_bytes: u64,
    /// 解密失败但仍继续写入的分片数量。
    undecrypted_segments: usize,
    started_at: Instant,
    last_event_at: Instant,
}

/// 已完成重试的响应，读取响应体期间仍需持有并发许可。
struct SegmentResponse {
    response: reqwest::Response,
    _permit: OwnedSemaphorePermit,
}

pub async fn run_task(task: DownloadTask) -> Result<TaskSnapshot, CoreError> {
    let DownloadTask {
        mut manifest,
        settings,
        event_sender,
        cancellation_token,
        global_permits,
    } = task;

    if cancellation_token.is_cancelled() {
        return Err(CoreError::Canceled);
    }

    let request_headers = validate_headers(manifest.request_headers.clone())?;
    let fetcher = Arc::new(PlaylistFetcher::new(&settings, request_headers)?);

    if manifest.playlist.is_none() {
        emit_snapshot(
            &event_sender,
            base_snapshot(&manifest, TaskStatus::Downloading, "正在解析播放列表"),
        );
        let playlist = fetcher.fetch_media_playlist(&manifest.source_url).await?;
        manifest.playlist = Some(playlist);
        manifest.save()?;
    }

    let playlist = manifest
        .playlist
        .clone()
        .ok_or(CoreError::InvalidPlaylist)?;
    let total_segments = playlist.segments.len();
    let initial_completed = manifest.completed_segment_count()?;
    let progress = Arc::new(Mutex::new(DownloadProgress {
        completed: initial_completed,
        downloaded_bytes: 0,
        undecrypted_segments: 0,
        started_at: Instant::now(),
        last_event_at: Instant::now() - Duration::from_millis(200),
    }));

    emit_snapshot(
        &event_sender,
        progress_snapshot(
            &manifest,
            &progress,
            TaskStatus::Downloading,
            "正在下载分片",
        ),
    );

    let key_cache = download_keys(&fetcher, &playlist.segments).await?;
    if let Some(initialization) = &playlist.initialization {
        let initialization_path = manifest.initialization_path();
        if !initialization_path.is_file() {
            let fetched = fetch_with_retries(
                &fetcher,
                &initialization.url,
                initialization.byte_range,
                &cancellation_token,
                &global_permits,
            )
            .await?;
            let data = fetched
                .response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|_| CoreError::Network("读取初始化段失败".into()))?;
            write_atomic(&initialization_path, &data).await?;
        }
    }

    let head_limit = total_segments * settings.tail_threshold as usize / 100;
    let (head, tail) = playlist.segments.split_at(head_limit);
    let base_workers = manifest.max_workers.clamp(1, 64);
    let tail_workers = (base_workers * settings.tail_boost as usize).clamp(1, 64);

    download_segments(
        head,
        base_workers,
        &manifest,
        &fetcher,
        &key_cache,
        &cancellation_token,
        &global_permits,
        &progress,
        &event_sender,
    )
    .await?;
    if cancellation_token.is_cancelled() {
        return Err(CoreError::Canceled);
    }
    download_segments(
        tail,
        tail_workers,
        &manifest,
        &fetcher,
        &key_cache,
        &cancellation_token,
        &global_permits,
        &progress,
        &event_sender,
    )
    .await?;
    if cancellation_token.is_cancelled() {
        return Err(CoreError::Canceled);
    }

    emit_snapshot(
        &event_sender,
        progress_snapshot(
            &manifest,
            &progress,
            TaskStatus::Downloading,
            "正在合并分片",
        ),
    );

    let segment_paths: Vec<PathBuf> = playlist
        .segments
        .iter()
        .map(|segment| manifest.segment_path(segment.index))
        .collect();
    let ffmpeg_program = crate::ffmpeg::detect_ffmpeg(&settings)
        .await
        .map(|info| info.path);
    // 没有 ffmpeg 时只能保留 TS，直接转换成 MP4 会在合并阶段失败。
    let initialization = manifest
        .playlist
        .as_ref()
        .and_then(|playlist| playlist.initialization.as_ref())
        .map(|_| manifest.initialization_path());
    let merge_result = merge_segments(
        &segment_paths,
        initialization.as_deref(),
        &manifest.output_directory,
        &manifest.output_name,
        ffmpeg_program.is_some(),
        ffmpeg_program.as_deref(),
    )
    .await?;

    manifest.mark_completed(merge_result.output_path.clone())?;

    if settings.auto_cleanup && !settings.keep_temp {
        let task_directory = manifest.task_directory();
        if let Err(error) = safe_remove_directory(&task_directory) {
            emit_log(
                &event_sender,
                CoreLogLevel::Warning,
                format!("清理临时文件失败：{error}"),
            );
        }
    }

    let undecrypted_segments = progress
        .lock()
        .map(|progress| progress.undecrypted_segments)
        .unwrap_or(0);
    let completion_message = if undecrypted_segments == 0 {
        merge_result.message.clone()
    } else {
        format!(
            "{}，但 {} 个分片解密失败，输出文件可能损坏",
            merge_result.message, undecrypted_segments
        )
    };
    if undecrypted_segments > 0 {
        emit_log(
            &event_sender,
            CoreLogLevel::Error,
            format!(
                "任务完成但存在解密失败：{}，共 {} 个分片，原始密文已保存到 _debug 目录",
                manifest.output_name, undecrypted_segments
            ),
        );
    }

    let mut snapshot = base_snapshot(&manifest, TaskStatus::Completed, &completion_message);
    snapshot.completed_segments = total_segments;
    snapshot.total_segments = total_segments;
    snapshot.progress = 1.0;
    snapshot.speed_bytes_per_second = 0;
    snapshot.estimated_seconds_remaining = 0;
    snapshot.output_path = Some(merge_result.output_path.to_string_lossy().into_owned());
    emit_snapshot(&event_sender, snapshot.clone());
    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
async fn download_segments(
    segments: &[MediaSegment],
    workers: usize,
    manifest: &TaskManifest,
    fetcher: &Arc<PlaylistFetcher>,
    key_cache: &Arc<HashMap<String, [u8; 16]>>,
    cancellation_token: &CancellationToken,
    global_permits: &Arc<Semaphore>,
    progress: &Arc<Mutex<DownloadProgress>>,
    event_sender: &UnboundedSender<TaskEvent>,
) -> Result<(), CoreError> {
    let remaining: Vec<MediaSegment> = segments
        .iter()
        .filter(|segment| !manifest.segment_path(segment.index).is_file())
        .cloned()
        .collect();
    if remaining.is_empty() {
        return Ok(());
    }

    stream::iter(remaining)
        .map(|segment| {
            let manifest = manifest.clone();
            let fetcher = fetcher.clone();
            let key_cache = key_cache.clone();
            let cancellation_token = cancellation_token.clone();
            let global_permits = global_permits.clone();
            let progress = progress.clone();
            let event_sender = event_sender.clone();
            async move {
                download_one_segment(
                    &manifest,
                    &fetcher,
                    &key_cache,
                    &cancellation_token,
                    &global_permits,
                    &progress,
                    &event_sender,
                    segment,
                )
                .await
            }
        })
        .buffer_unordered(workers)
        .try_for_each(|_| ready(Ok(())))
        .await
}

#[allow(clippy::too_many_arguments)]
async fn download_one_segment(
    manifest: &TaskManifest,
    fetcher: &Arc<PlaylistFetcher>,
    key_cache: &Arc<HashMap<String, [u8; 16]>>,
    cancellation_token: &CancellationToken,
    global_permits: &Arc<Semaphore>,
    progress: &Arc<Mutex<DownloadProgress>>,
    event_sender: &UnboundedSender<TaskEvent>,
    segment: MediaSegment,
) -> Result<(), CoreError> {
    if cancellation_token.is_cancelled() {
        return Err(CoreError::Canceled);
    }
    let fetched = fetch_with_retries(
        fetcher,
        &segment.url,
        segment.byte_range,
        cancellation_token,
        global_permits,
    )
    .await?;

    // 未加密的大分片直接流式落盘，避免整片驻留内存。
    let stream_to_disk = segment.encryption.is_none()
        && fetched.response.content_length().unwrap_or(0) > STREAM_THRESHOLD_BYTES;
    if stream_to_disk {
        let size =
            stream_segment_to_disk(fetched.response, &manifest.segment_path(segment.index)).await?;
        record_progress(manifest, progress, event_sender, size)?;
        return Ok(());
    }

    let mut data = fetched
        .response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|_| CoreError::Network("读取分片失败".into()))?;

    let mut decrypted = true;
    if let Some(encryption) = &segment.encryption {
        let key = key_cache
            .get(&encryption.key_uri)
            .copied()
            .ok_or(CoreError::InvalidKey)?;
        let iv = encryption.iv.unwrap_or_else(|| {
            manifest
                .playlist
                .as_ref()
                .map(|playlist| implicit_iv(playlist.media_sequence, segment.index))
                .unwrap_or_else(|| implicit_iv(0, segment.index))
        });
        match decrypt_aes128_cbc(&data, &key, iv) {
            Ok(result) => data = result,
            Err(error) => {
                // 解密失败不中断整个任务：保留原始密文供排查，继续下载其余分片。
                decrypted = false;
                if let Ok(mut progress) = progress.lock() {
                    progress.undecrypted_segments += 1;
                }
                let _ = tokio::fs::create_dir_all(manifest.debug_directory()).await;
                let _ = write_atomic(&manifest.debug_path(segment.index), &data).await;
                emit_log(
                    event_sender,
                    CoreLogLevel::Error,
                    format!(
                        "分片 {} 解密失败：{}，已保留原始密文",
                        segment.index + 1,
                        error.user_message()
                    ),
                );
            }
        }
    }

    match detect_format(&data) {
        SegmentFormat::Ts | SegmentFormat::Fmp4 => {}
        other => {
            if decrypted {
                return Err(CoreError::InvalidSegment(diagnostic_message(&data)));
            }
            emit_log(
                event_sender,
                CoreLogLevel::Warning,
                format!(
                    "分片 {} 解密失败，内容为{}",
                    segment.index + 1,
                    other.label()
                ),
            );
        }
    }

    write_atomic(&manifest.segment_path(segment.index), &data).await?;
    record_progress(manifest, progress, event_sender, data.len() as u64)?;
    Ok(())
}

fn record_progress(
    manifest: &TaskManifest,
    progress: &Arc<Mutex<DownloadProgress>>,
    event_sender: &UnboundedSender<TaskEvent>,
    bytes: u64,
) -> Result<(), CoreError> {
    let should_emit;
    {
        let mut progress = progress
            .lock()
            .map_err(|_| CoreError::Io("更新进度失败".into()))?;
        progress.completed += 1;
        progress.downloaded_bytes += bytes;
        should_emit = progress.last_event_at.elapsed() >= Duration::from_millis(100);
        if should_emit {
            progress.last_event_at = Instant::now();
        }
    }
    if should_emit {
        let snapshot =
            progress_snapshot(manifest, progress, TaskStatus::Downloading, "正在下载分片");
        emit_snapshot(event_sender, snapshot);
    }
    Ok(())
}

/// 边下载边写入磁盘，只在内存中保留用于格式识别的头部样本。
async fn stream_segment_to_disk(
    response: reqwest::Response,
    path: &Path,
) -> Result<u64, CoreError> {
    let part_path = path.with_extension("part");
    if part_path.exists() {
        tokio::fs::remove_file(&part_path)
            .await
            .map_err(|_| CoreError::Io("清理临时分片失败".into()))?;
    }
    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|_| CoreError::Io("创建分片失败".into()))?;
    let mut header = Vec::with_capacity(HEADER_SAMPLE_BYTES);
    let mut total = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CoreError::Network("下载分片中断".into()))?;
        if header.len() < HEADER_SAMPLE_BYTES {
            let take = (HEADER_SAMPLE_BYTES - header.len()).min(chunk.len());
            header.extend_from_slice(&chunk[..take]);
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| CoreError::Io("写入分片失败".into()))?;
        total += chunk.len() as u64;
    }
    file.flush()
        .await
        .map_err(|_| CoreError::Io("写入分片失败".into()))?;
    drop(file);

    match detect_format(&header) {
        SegmentFormat::Ts | SegmentFormat::Fmp4 => {}
        other => {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(CoreError::InvalidSegment(format!(
                "分片内容异常，检测到{}",
                other.label()
            )));
        }
    }
    tokio::fs::rename(&part_path, path)
        .await
        .map_err(|_| CoreError::Io("保存分片失败".into()))?;
    Ok(total)
}

async fn fetch_with_retries(
    fetcher: &Arc<PlaylistFetcher>,
    url: &str,
    byte_range: Option<ByteRange>,
    cancellation_token: &CancellationToken,
    global_permits: &Arc<Semaphore>,
) -> Result<SegmentResponse, CoreError> {
    let range = byte_range.map(|range| {
        let offset = range.offset.unwrap_or(0);
        format!(
            "bytes={}-{}",
            offset,
            offset + range.length.saturating_sub(1)
        )
    });
    let mut attempt = 0;
    loop {
        if cancellation_token.is_cancelled() {
            return Err(CoreError::Canceled);
        }
        let permit = tokio::select! {
            permit = global_permits.clone().acquire_owned() => {
                permit.map_err(|_| CoreError::Io("获取下载并发许可失败".into()))?
            }
            _ = cancellation_token.cancelled() => return Err(CoreError::Canceled),
        };
        let result = fetcher.send_raw(url, range.clone()).await;
        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                if (200..300).contains(&status) {
                    return Ok(SegmentResponse {
                        response,
                        _permit: permit,
                    });
                }
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok());
                if status != 429 && !(500..=599).contains(&status) {
                    return status_error(status);
                }
                if attempt >= MAX_AUTO_RETRIES {
                    return status_error(status);
                }
                attempt += 1;
                let delay = if status == 429 {
                    parse_retry_after(retry_after)
                } else {
                    Duration::from_millis(500 * attempt as u64)
                };
                tokio::select! {
                    _ = sleep(delay) => {}
                    _ = cancellation_token.cancelled() => return Err(CoreError::Canceled),
                }
            }
            Err(error) => {
                if attempt >= MAX_AUTO_RETRIES {
                    return Err(error);
                }
                attempt += 1;
                let delay = Duration::from_millis(500 * attempt as u64);
                tokio::select! {
                    _ = sleep(delay) => {}
                    _ = cancellation_token.cancelled() => return Err(CoreError::Canceled),
                }
            }
        }
    }
}

fn status_error(status: u16) -> Result<SegmentResponse, CoreError> {
    Err(match status {
        403 => CoreError::Forbidden,
        404 => CoreError::NotFound,
        429 => CoreError::TooManyRequests,
        500..=599 => CoreError::ServerError(status),
        status => CoreError::HttpStatus { status },
    })
}

async fn download_keys(
    fetcher: &Arc<PlaylistFetcher>,
    segments: &[MediaSegment],
) -> Result<Arc<HashMap<String, [u8; 16]>>, CoreError> {
    let mut cache = HashMap::new();
    for segment in segments {
        let Some(encryption) = &segment.encryption else {
            continue;
        };
        if cache.contains_key(&encryption.key_uri) {
            continue;
        }
        let key = fetcher.fetch_key(&encryption.key_uri).await?;
        let key: [u8; 16] = key.try_into().map_err(|_| CoreError::InvalidKey)?;
        cache.insert(encryption.key_uri.clone(), key);
    }
    Ok(Arc::new(cache))
}

async fn write_atomic(path: &PathBuf, data: &[u8]) -> Result<(), CoreError> {
    let part_path = path.with_extension("part");
    if part_path.exists() {
        tokio::fs::remove_file(&part_path)
            .await
            .map_err(|_| CoreError::Io("清理临时分片失败".into()))?;
    }
    tokio::fs::write(&part_path, data)
        .await
        .map_err(|_| CoreError::Io("写入分片失败".into()))?;
    tokio::fs::rename(&part_path, path)
        .await
        .map_err(|_| CoreError::Io("保存分片失败".into()))
}

fn base_snapshot(manifest: &TaskManifest, status: TaskStatus, detail: &str) -> TaskSnapshot {
    let total_segments = manifest.total_segment_count();
    let completed_segments = manifest.completed_segment_count().unwrap_or(0);
    TaskSnapshot {
        id: manifest.id,
        source_url: manifest.source_url.clone(),
        output_name: manifest.output_name.clone(),
        output_directory: manifest.output_directory.to_string_lossy().into_owned(),
        request_headers: manifest.request_headers_json(),
        status,
        completed_segments,
        total_segments,
        progress: if total_segments == 0 {
            0.0
        } else {
            completed_segments as f32 / total_segments as f32
        },
        speed_bytes_per_second: 0,
        estimated_seconds_remaining: 0,
        detail: detail.to_string(),
        output_path: None,
    }
}

fn progress_snapshot(
    manifest: &TaskManifest,
    progress: &Arc<Mutex<DownloadProgress>>,
    status: TaskStatus,
    detail: &str,
) -> TaskSnapshot {
    let Ok(progress) = progress.lock() else {
        return base_snapshot(manifest, status, detail);
    };
    let total_segments = manifest.total_segment_count().max(progress.completed);
    let elapsed = progress.started_at.elapsed().as_secs_f64().max(0.001);
    let speed = (progress.downloaded_bytes as f64 / elapsed) as u64;
    let remaining_segments = total_segments.saturating_sub(progress.completed);
    let remaining_bytes = if progress.completed > 0 {
        let average_segment_bytes = progress.downloaded_bytes / progress.completed as u64;
        remaining_segments as u64 * average_segment_bytes
    } else {
        0
    };
    let remaining_seconds = remaining_bytes.checked_div(speed).unwrap_or(0);
    TaskSnapshot {
        id: manifest.id,
        source_url: manifest.source_url.clone(),
        output_name: manifest.output_name.clone(),
        output_directory: manifest.output_directory.to_string_lossy().into_owned(),
        request_headers: manifest.request_headers_json(),
        status,
        completed_segments: progress.completed,
        total_segments,
        progress: if total_segments == 0 {
            0.0
        } else {
            progress.completed as f32 / total_segments as f32
        },
        speed_bytes_per_second: speed,
        estimated_seconds_remaining: remaining_seconds,
        detail: detail.to_string(),
        output_path: None,
    }
}

fn emit_snapshot(sender: &UnboundedSender<TaskEvent>, snapshot: TaskSnapshot) {
    let _ = sender.send(TaskEvent::Snapshot(snapshot));
}

fn emit_log(sender: &UnboundedSender<TaskEvent>, level: CoreLogLevel, message: String) {
    let _ = sender.send(TaskEvent::Log { level, message });
}
