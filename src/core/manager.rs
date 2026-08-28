use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use tokio::{
    runtime::Runtime,
    sync::{mpsc, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    config::Settings,
    core::{
        downloader::{run_task, DownloadTask},
        error::CoreError,
        events::{CoreLogLevel, NewTask, TaskCommand, TaskEvent, TaskSnapshot, TaskStatus},
        headers::parse_header_json,
        merge::sanitize_filename,
        merge::{merge_segments, safe_remove_directory, scan_merge_folder},
        task::{discover_task_manifests, TaskManifest, TaskRegistry},
    },
};

pub struct TaskManager {
    _runtime: Runtime,
    command_sender: mpsc::UnboundedSender<TaskCommand>,
    event_receiver: Arc<Mutex<mpsc::UnboundedReceiver<TaskEvent>>>,
    next_request_id: AtomicU64,
}

impl TaskManager {
    pub fn new(settings: Settings, task_registry_path: PathBuf) -> Self {
        let runtime = Runtime::new().expect("创建异步运行时失败");
        let (command_sender, command_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let maximum_workers = settings.max_workers * settings.tail_boost as usize;
        let task_permits = Arc::new(Semaphore::new(settings.max_concurrent_downloads));
        let global_permits = Arc::new(Semaphore::new(
            settings.max_concurrent_downloads * maximum_workers,
        ));
        let settings = Arc::new(RwLock::new(settings));

        runtime.spawn(manager_loop(
            command_receiver,
            event_sender,
            settings,
            task_permits,
            global_permits,
            task_registry_path,
        ));

        Self {
            _runtime: runtime,
            command_sender,
            event_receiver: Arc::new(Mutex::new(event_receiver)),
            next_request_id: AtomicU64::new(1),
        }
    }

    pub fn send(&self, command: TaskCommand) {
        if self.command_sender.send(command).is_err() {
            tracing::warn!("任务管理器已停止");
        }
    }

    pub fn try_recv_event(&self) -> Option<TaskEvent> {
        self.event_receiver
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.try_recv().ok())
    }

    pub fn resume_tasks(&self, directories: Vec<PathBuf>) {
        self.send(TaskCommand::ResumeTasks(directories));
    }

    pub fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct TaskRuntime {
    manifest: TaskManifest,
    snapshot: TaskSnapshot,
    run_id: u64,
    cancellation_token: CancellationToken,
    handle: JoinHandle<()>,
}

struct ManagerState {
    tasks: Arc<Mutex<HashMap<u64, TaskRuntime>>>,
    settings: Arc<RwLock<Settings>>,
    task_permits: Arc<Semaphore>,
    global_permits: Arc<Semaphore>,
    event_sender: mpsc::UnboundedSender<TaskEvent>,
    next_id: AtomicU64,
    next_run_id: AtomicU64,
    task_registry_path: PathBuf,
}

// 配置锁理论上不会中毒，但管理器循环一旦 panic 会导致所有任务失控，因此做降级处理。
fn read_settings(settings: &RwLock<Settings>) -> Settings {
    match settings.read() {
        Ok(settings) => settings.clone(),
        Err(error) => error.into_inner().clone(),
    }
}

async fn manager_loop(
    mut command_receiver: mpsc::UnboundedReceiver<TaskCommand>,
    event_sender: mpsc::UnboundedSender<TaskEvent>,
    settings: Arc<RwLock<Settings>>,
    task_permits: Arc<Semaphore>,
    global_permits: Arc<Semaphore>,
    task_registry_path: PathBuf,
) {
    let tasks = Arc::new(Mutex::new(HashMap::<u64, TaskRuntime>::new()));
    let state = ManagerState {
        tasks,
        settings,
        task_permits,
        global_permits,
        event_sender,
        next_id: AtomicU64::new(1),
        next_run_id: AtomicU64::new(1),
        task_registry_path,
    };

    while let Some(command) = command_receiver.recv().await {
        match command {
            TaskCommand::Add(new_task) => add_task(&state, new_task),
            TaskCommand::Start(id) => start_task(&state, id),
            TaskCommand::StartAll => start_all_tasks(&state),
            TaskCommand::Cancel(id) => cancel_task(&state, id),
            TaskCommand::Retry(id) => start_task(&state, id),
            TaskCommand::Delete(id) => delete_task(&state, id),
            TaskCommand::EditTask {
                id,
                source_url,
                output_name,
            } => edit_task(&state, id, source_url, output_name),
            TaskCommand::ClearCompleted => clear_completed_tasks(&state),
            TaskCommand::ResumeTasks(directories) => resume_tasks(&state, directories),
            TaskCommand::UpdateSettings(new_settings) => update_settings(&state, new_settings),
            TaskCommand::DetectFfmpeg => {
                let settings = read_settings(&state.settings);
                let path = crate::ffmpeg::detect_ffmpeg(&settings).await;
                let _ = state.event_sender.send(TaskEvent::FfmpegStatus { path });
            }
            TaskCommand::ScanMergeFolder { request_id, folder } => {
                let result = scan_merge_folder(&folder)
                    .await
                    .map_err(|error| error.user_message());
                let _ = state
                    .event_sender
                    .send(TaskEvent::MergeScan { request_id, result });
            }
            TaskCommand::MergeFolder {
                request_id,
                folder,
                output_name,
                convert_to_mp4,
            } => {
                // 合并可能持续很久，必须在独立协程中执行，否则会阻塞整个管理循环。
                let settings = state.settings.clone();
                let event_sender = state.event_sender.clone();
                tokio::spawn(async move {
                    let result = merge_folder(&settings, folder, output_name, convert_to_mp4).await;
                    let _ = event_sender.send(TaskEvent::MergeFinished { request_id, result });
                });
            }
        }
    }
}

fn add_task(state: &ManagerState, new_task: NewTask) {
    if Url::parse(&new_task.source_url).is_err() {
        send_log_and_toast(
            &state.event_sender,
            CoreLogLevel::Error,
            "任务添加失败：M3U8 链接无效".to_string(),
        );
        return;
    }
    let request_headers = match parse_header_json(&new_task.request_headers) {
        Ok(headers) => headers,
        Err(error) => {
            send_log_and_toast(
                &state.event_sender,
                CoreLogLevel::Error,
                format!("任务添加失败：{}", error.user_message()),
            );
            return;
        }
    };
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let manifest = match TaskManifest::new(
        id,
        &new_task.source_url,
        &new_task.output_name,
        &new_task.output_directory,
        new_task.max_workers,
        request_headers,
    ) {
        Ok(manifest) => manifest,
        Err(error) => {
            send_log_and_toast(
                &state.event_sender,
                CoreLogLevel::Error,
                format!("任务添加失败：{}", error.user_message()),
            );
            return;
        }
    };
    if let Err(error) =
        TaskRegistry::register(&state.task_registry_path, &manifest.output_directory)
    {
        let _ = state.event_sender.send(TaskEvent::Log {
            level: CoreLogLevel::Warning,
            message: format!("任务注册表更新失败：{}", error.user_message()),
        });
    }
    let snapshot = initial_snapshot(&manifest);
    let _ = state
        .event_sender
        .send(TaskEvent::Snapshot(snapshot.clone()));
    let _ = state.event_sender.send(TaskEvent::Log {
        level: CoreLogLevel::Info,
        message: format!("任务已添加：{}", manifest.output_name),
    });
    spawn_task(state, manifest, snapshot);
}

fn spawn_task(state: &ManagerState, manifest: TaskManifest, mut snapshot: TaskSnapshot) {
    let tasks = state.tasks.clone();
    let settings = state.settings.clone();
    let task_permits = state.task_permits.clone();
    let global_permits = state.global_permits.clone();
    let event_sender = state.event_sender.clone();
    let cancellation_token = CancellationToken::new();
    let id = manifest.id;
    let run_id = state.next_run_id.fetch_add(1, Ordering::Relaxed);
    let run_manifest = manifest.clone();
    let run_token = cancellation_token.clone();

    snapshot.status = TaskStatus::Waiting;
    snapshot.detail = "等待下载槽位".to_string();
    let _ = event_sender.send(TaskEvent::Snapshot(snapshot.clone()));

    let handle = tokio::spawn(async move {
        let task_permit = tokio::select! {
            permit = task_permits.acquire() => {
                permit.map_err(|_| CoreError::Io("获取任务并发许可失败".into()))
            }
            _ = run_token.cancelled() => Err(CoreError::Canceled),
        };
        let task_permit = match task_permit {
            Ok(task_permit) => task_permit,
            Err(_) => {
                finish_task(&tasks, id, run_id, Err(CoreError::Canceled), &event_sender);
                return;
            }
        };
        let settings = read_settings(&settings);
        let result = run_task(DownloadTask {
            manifest: run_manifest,
            settings,
            event_sender: event_sender.clone(),
            cancellation_token: run_token.clone(),
            global_permits,
        })
        .await;
        drop(task_permit);
        finish_task(&tasks, id, run_id, result, &event_sender);
    });

    if let Ok(mut tasks) = state.tasks.lock() {
        tasks.insert(
            id,
            TaskRuntime {
                manifest,
                snapshot,
                run_id,
                cancellation_token,
                handle,
            },
        );
    }
}

fn finish_task(
    tasks: &Arc<Mutex<HashMap<u64, TaskRuntime>>>,
    id: u64,
    run_id: u64,
    result: Result<TaskSnapshot, CoreError>,
    event_sender: &mpsc::UnboundedSender<TaskEvent>,
) {
    // 任务可能已被删除，或已被重新开始的运行取代，此时应丢弃过期的结束事件。
    let is_current_run = tasks
        .lock()
        .map(|tasks| {
            tasks
                .get(&id)
                .is_some_and(|runtime| runtime.run_id == run_id)
        })
        .unwrap_or(false);
    if !is_current_run {
        return;
    }

    let final_snapshot = match result {
        Ok(snapshot) => Some(snapshot),
        Err(CoreError::Canceled) => Some(TaskSnapshot {
            status: TaskStatus::Canceled,
            detail: "任务已取消".to_string(),
            ..task_snapshot(tasks, id)
        }),
        Err(error) => Some(TaskSnapshot {
            status: TaskStatus::Failed,
            detail: error.user_message(),
            ..task_snapshot(tasks, id)
        }),
    };
    let Some(snapshot) = final_snapshot else {
        return;
    };
    if let Ok(mut tasks) = tasks.lock() {
        if let Some(runtime) = tasks.get_mut(&id) {
            runtime.snapshot = snapshot.clone();
        }
    }
    let _ = event_sender.send(TaskEvent::Snapshot(snapshot.clone()));
    let (level, message) = match snapshot.status {
        TaskStatus::Completed => (
            CoreLogLevel::Info,
            format!("任务完成：{}", snapshot.output_name),
        ),
        TaskStatus::Canceled => (
            CoreLogLevel::Warning,
            format!("任务已取消：{}", snapshot.output_name),
        ),
        TaskStatus::Failed => (
            CoreLogLevel::Error,
            format!("任务失败：{}，{}", snapshot.output_name, snapshot.detail),
        ),
        _ => (
            CoreLogLevel::Warning,
            format!(
                "任务结束：{}，{}",
                snapshot.output_name,
                snapshot.status.label()
            ),
        ),
    };
    let _ = event_sender.send(TaskEvent::Log { level, message });
    let _ = event_sender.send(TaskEvent::Toast {
        message: if snapshot.status == TaskStatus::Completed {
            format!("下载完成：{}", snapshot.output_name)
        } else {
            format!("任务失败：{}", snapshot.output_name)
        },
        error: snapshot.status != TaskStatus::Completed,
    });
}

fn task_snapshot(tasks: &Arc<Mutex<HashMap<u64, TaskRuntime>>>, id: u64) -> TaskSnapshot {
    tasks
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(&id).map(|runtime| runtime.snapshot.clone()))
        .unwrap_or_else(|| TaskSnapshot {
            id,
            source_url: String::new(),
            output_name: String::new(),
            output_directory: String::new(),
            status: TaskStatus::Failed,
            completed_segments: 0,
            total_segments: 0,
            progress: 0.0,
            speed_bytes_per_second: 0,
            estimated_seconds_remaining: 0,
            detail: "任务状态丢失".to_string(),
            output_path: None,
        })
}

fn start_task(state: &ManagerState, id: u64) {
    if let Ok(mut tasks) = state.tasks.lock() {
        let Some(runtime) = tasks.get_mut(&id) else {
            return;
        };
        if runtime.snapshot.status.is_active() {
            return;
        }
        if runtime.manifest.completed {
            runtime.manifest.completed = false;
            runtime.manifest.output_path = None;
            if let Err(error) = runtime.manifest.save() {
                let message = format!("重试失败：{}", error.user_message());
                let _ = state.event_sender.send(TaskEvent::Log {
                    level: CoreLogLevel::Error,
                    message: message.clone(),
                });
                let _ = state.event_sender.send(TaskEvent::Toast {
                    message,
                    error: true,
                });
                return;
            }
        }
        let manifest = runtime.manifest.clone();
        let snapshot = runtime.snapshot.clone();
        runtime.handle.abort();
        drop(tasks);
        spawn_task(state, manifest, snapshot);
    }
}

fn start_all_tasks(state: &ManagerState) {
    let ids: Vec<u64> = state
        .tasks
        .lock()
        .map(|tasks| {
            tasks
                .values()
                .filter(|runtime| !runtime.snapshot.status.is_active())
                .map(|runtime| runtime.manifest.id)
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        start_task(state, id);
    }
}

fn cancel_task(state: &ManagerState, id: u64) {
    if let Ok(mut tasks) = state.tasks.lock() {
        let Some(runtime) = tasks.get_mut(&id) else {
            return;
        };
        if runtime.snapshot.status.is_active() {
            runtime.cancellation_token.cancel();
            runtime.snapshot.status = TaskStatus::Canceling;
            runtime.snapshot.detail = "正在取消任务".to_string();
            let snapshot = runtime.snapshot.clone();
            drop(tasks);
            let _ = state.event_sender.send(TaskEvent::Snapshot(snapshot));
        }
    }
}

fn delete_task(state: &ManagerState, id: u64) {
    let removed = state
        .tasks
        .lock()
        .ok()
        .and_then(|mut tasks| tasks.remove(&id));
    let Some(runtime) = removed else {
        return;
    };
    runtime.cancellation_token.cancel();
    runtime.handle.abort();
    // 删除任务需要同时清理临时目录，否则重启后会被当作未完成任务重新载入。
    if let Err(error) = safe_remove_directory(&runtime.manifest.task_directory()) {
        let _ = state.event_sender.send(TaskEvent::Log {
            level: CoreLogLevel::Warning,
            message: format!("清理临时文件失败：{}", error.user_message()),
        });
    }
    let _ = state.event_sender.send(TaskEvent::Log {
        level: CoreLogLevel::Info,
        message: format!("任务已删除：{}", runtime.manifest.output_name),
    });
}

fn edit_task(state: &ManagerState, id: u64, source_url: String, output_name: String) {
    if Url::parse(&source_url).is_err() {
        send_log_and_toast(
            &state.event_sender,
            CoreLogLevel::Error,
            "编辑失败：链接无效".to_string(),
        );
        return;
    }
    if let Ok(mut tasks) = state.tasks.lock() {
        let Some(runtime) = tasks.get_mut(&id) else {
            return;
        };
        if runtime.snapshot.status.is_active() {
            send_log_and_toast(
                &state.event_sender,
                CoreLogLevel::Warning,
                "下载中的任务不能编辑".to_string(),
            );
            return;
        }
        runtime.manifest.source_url = source_url;
        runtime.manifest.output_name = sanitize_filename(&output_name);
        if let Err(error) = runtime.manifest.save() {
            send_log_and_toast(
                &state.event_sender,
                CoreLogLevel::Error,
                format!("编辑失败：{}", error.user_message()),
            );
            return;
        }
        runtime.snapshot.source_url = runtime.manifest.source_url.clone();
        runtime.snapshot.output_name = runtime.manifest.output_name.clone();
        let snapshot = runtime.snapshot.clone();
        drop(tasks);
        let _ = state.event_sender.send(TaskEvent::Snapshot(snapshot));
        let _ = state.event_sender.send(TaskEvent::Log {
            level: CoreLogLevel::Info,
            message: "任务已更新".to_string(),
        });
    }
}

fn clear_completed_tasks(state: &ManagerState) {
    if let Ok(mut tasks) = state.tasks.lock() {
        tasks.retain(|_, runtime| runtime.snapshot.status != TaskStatus::Completed);
    }
}

fn resume_tasks(state: &ManagerState, directories: Vec<PathBuf>) {
    let mut manifests = Vec::new();
    for directory in directories {
        manifests.extend(discover_task_manifests(&directory));
    }
    manifests.sort_by_key(|manifest| manifest.id);
    manifests.dedup_by_key(|manifest| manifest.id);
    if manifests.is_empty() {
        return;
    }
    for manifest in manifests {
        if manifest.completed {
            continue;
        }
        if state.next_id.load(Ordering::Relaxed) <= manifest.id {
            state.next_id.store(manifest.id + 1, Ordering::Relaxed);
        }
        let snapshot = initial_snapshot(&manifest);
        let _ = state
            .event_sender
            .send(TaskEvent::Snapshot(snapshot.clone()));
        let _ = state.event_sender.send(TaskEvent::Log {
            level: CoreLogLevel::Info,
            message: format!("发现未完成任务：{}", manifest.output_name),
        });
        spawn_task(state, manifest, snapshot);
    }
}

fn update_settings(state: &ManagerState, settings: Settings) {
    let mut current = match state.settings.write() {
        Ok(current) => current,
        Err(error) => error.into_inner(),
    };
    *current = settings;
    drop(current);
    let _ = state.event_sender.send(TaskEvent::Log {
        level: CoreLogLevel::Info,
        message: "设置已更新，新任务将使用新配置".to_string(),
    });
}

fn initial_snapshot(manifest: &TaskManifest) -> TaskSnapshot {
    let total_segments = manifest.total_segment_count();
    let completed_segments = manifest.completed_segment_count().unwrap_or(0);
    TaskSnapshot {
        id: manifest.id,
        source_url: manifest.source_url.clone(),
        output_name: manifest.output_name.clone(),
        output_directory: manifest.output_directory.to_string_lossy().into_owned(),
        status: TaskStatus::Waiting,
        completed_segments,
        total_segments,
        progress: if total_segments == 0 {
            0.0
        } else {
            completed_segments as f32 / total_segments as f32
        },
        speed_bytes_per_second: 0,
        estimated_seconds_remaining: 0,
        detail: "等待下载槽位".to_string(),
        output_path: None,
    }
}

async fn merge_folder(
    settings: &RwLock<Settings>,
    folder: std::path::PathBuf,
    output_name: String,
    convert_to_mp4: bool,
) -> Result<crate::core::merge::MergeResult, String> {
    let settings = read_settings(settings);
    let scan = scan_merge_folder(&folder)
        .await
        .map_err(|error| error.user_message())?;
    let ffmpeg_program = crate::ffmpeg::detect_ffmpeg(&settings).await;
    let (segments, initialization) =
        match (scan.ts_segments.is_empty(), scan.fmp4_segments.is_empty()) {
            (false, true) => (scan.ts_segments, None),
            (true, false) => (scan.fmp4_segments, scan.initialization),
            (false, false) => return Err("文件夹中同时包含 TS 和 fMP4 分片，无法合并".to_string()),
            (true, true) => return Err("文件夹中没有找到 TS 或 fMP4 分片".to_string()),
        };
    merge_segments(
        &segments,
        initialization.as_deref(),
        &folder,
        &output_name,
        convert_to_mp4,
        ffmpeg_program.as_deref(),
    )
    .await
    .map_err(|error| error.user_message())
}

fn send_log_and_toast(
    sender: &mpsc::UnboundedSender<TaskEvent>,
    level: CoreLogLevel,
    message: String,
) {
    let error = level == CoreLogLevel::Error;
    let _ = sender.send(TaskEvent::Log {
        level,
        message: message.clone(),
    });
    let _ = sender.send(TaskEvent::Toast { message, error });
}
