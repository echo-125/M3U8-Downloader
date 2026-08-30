use std::{
    collections::HashMap,
    path::{Path, PathBuf},
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
            TaskCommand::Reset(ids) => {
                for id in ids {
                    reset_task(&state, id);
                }
            }
            TaskCommand::Retry(id) => start_task(&state, id),
            TaskCommand::Delete(id) => delete_task(&state, id),
            TaskCommand::RemoveFinished => remove_finished_tasks(&state),
            TaskCommand::EditTask {
                id,
                source_url,
                output_name,
                output_directory,
                request_headers,
            } => {
                edit_task(
                    &state,
                    id,
                    source_url,
                    output_name,
                    output_directory,
                    request_headers,
                )
                .await
            }
            TaskCommand::ClearFinished => clear_finished_tasks(&state),
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
    if new_task.auto_start {
        spawn_task(state, manifest, snapshot);
    } else {
        register_idle_task(state, manifest, snapshot);
    }
}

/// 把任务登记进任务表但不启动下载，保持「等待中」状态，等用户手动开始。
fn register_idle_task(state: &ManagerState, manifest: TaskManifest, mut snapshot: TaskSnapshot) {
    let id = manifest.id;
    snapshot.status = TaskStatus::Waiting;
    snapshot.detail = "等待下载槽位".to_string();
    let _ = state
        .event_sender
        .send(TaskEvent::Snapshot(snapshot.clone()));
    let run_id = state.next_run_id.fetch_add(1, Ordering::Relaxed);
    let cancellation_token = CancellationToken::new();
    let tasks = state.tasks.clone();
    let event_sender = state.event_sender.clone();
    let run_token = cancellation_token.clone();
    // 占位协程只响应取消：不开始下载。任务真正开始时会由 start_task 重建并替换句柄。
    let handle = tokio::spawn(async move {
        run_token.cancelled().await;
        finish_task(&tasks, id, run_id, Err(CoreError::Canceled), &event_sender);
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
            request_headers: String::new(),
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
        // 只接受可开始的状态：下载中/取消中不能重复启动；已完成的任务禁止重新
        // 开始——重下会重置 manifest 并覆盖已有成品（README 行为约定），界面已
        // 按 is_startable 过滤，这里是核心侧防线。等待中的任务（包括
        // auto_start=false 时 register_idle_task 登记的占位任务）由这里启动。
        if !runtime.snapshot.status.is_startable() {
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
                .filter(|runtime| runtime.snapshot.status.is_startable())
                .map(|runtime| runtime.manifest.id)
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        start_task(state, id);
    }
}

/// 重置任务：停止下载、删除已下载的分片、恢复「等待中」，不保留断点续传。
/// 已完成的任务不参与重置（成品不该被删）。
fn reset_task(state: &ManagerState, id: u64) {
    let pending = {
        let Ok(mut tasks) = state.tasks.lock() else {
            return;
        };
        let Some(runtime) = tasks.get_mut(&id) else {
            return;
        };
        if !matches!(
            runtime.snapshot.status,
            TaskStatus::Waiting
                | TaskStatus::Downloading
                | TaskStatus::Canceling
                | TaskStatus::Failed
        ) {
            // 已完成的任务不重置。
            return;
        }
        runtime.cancellation_token.cancel();
        runtime.handle.abort();
        // 删除已下载的分片，重置后从头下载。
        if let Err(error) = safe_remove_directory(&runtime.manifest.task_directory()) {
            let _ = state.event_sender.send(TaskEvent::Log {
                level: CoreLogLevel::Warning,
                message: format!("重置任务清理分片失败：{}", error.user_message()),
            });
        }
        runtime.manifest.completed = false;
        runtime.manifest.output_path = None;
        if let Err(error) = runtime.manifest.save() {
            let _ = state.event_sender.send(TaskEvent::Log {
                level: CoreLogLevel::Warning,
                message: format!("重置任务保存失败：{}", error.user_message()),
            });
        }
        Some((runtime.manifest.clone(), runtime.snapshot.clone()))
    };
    let Some((manifest, snapshot)) = pending else {
        return;
    };
    register_idle_task(state, manifest, snapshot);
}

/// 移除所有已完成和已失败的任务（界面「删除」按钮，无视勾选）。
fn remove_finished_tasks(state: &ManagerState) {
    let removed: Vec<TaskRuntime> = {
        let Ok(mut tasks) = state.tasks.lock() else {
            return;
        };
        let finished: Vec<u64> = tasks
            .values()
            .filter(|runtime| {
                matches!(
                    runtime.snapshot.status,
                    TaskStatus::Completed | TaskStatus::Failed
                )
            })
            .map(|runtime| runtime.manifest.id)
            .collect();
        finished.iter().filter_map(|id| tasks.remove(id)).collect()
    };
    if removed.is_empty() {
        return;
    }
    for runtime in &removed {
        runtime.cancellation_token.cancel();
        runtime.handle.abort();
        // 未完成的（失败）任务要打标记，避免重启后作为断点续传重新载入。
        if !runtime.manifest.completed {
            let mut manifest = runtime.manifest.clone();
            if let Err(error) = manifest.mark_dismissed() {
                let _ = state.event_sender.send(TaskEvent::Log {
                    level: CoreLogLevel::Warning,
                    message: format!("标记已删除任务失败：{}", error.user_message()),
                });
            }
        }
        // 清理临时分片目录。
        if let Err(error) = safe_remove_directory(&runtime.manifest.task_directory()) {
            let _ = state.event_sender.send(TaskEvent::Log {
                level: CoreLogLevel::Warning,
                message: format!("清理任务临时文件失败：{}", error.user_message()),
            });
        }
    }
    let ids: Vec<u64> = removed.iter().map(|runtime| runtime.manifest.id).collect();
    let _ = state.event_sender.send(TaskEvent::TasksRemoved { ids });
}

fn delete_task(state: &ManagerState, id: u64) {
    let removed = state
        .tasks
        .lock()
        .ok()
        .and_then(|mut tasks| tasks.remove(&id));
    let Some(runtime) = removed else {
        // 任务已不存在时也要通知界面移除对应行，否则会残留一个永远无法操作的幽灵任务。
        let _ = state
            .event_sender
            .send(TaskEvent::TasksRemoved { ids: vec![id] });
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
    let _ = state
        .event_sender
        .send(TaskEvent::TasksRemoved { ids: vec![id] });
}

async fn edit_task(
    state: &ManagerState,
    id: u64,
    source_url: String,
    output_name: String,
    output_directory: String,
    request_headers: String,
) {
    if Url::parse(&source_url).is_err() {
        send_log_and_toast(
            &state.event_sender,
            CoreLogLevel::Error,
            "编辑失败：链接无效".to_string(),
        );
        return;
    }
    let request_headers = match parse_header_json(&request_headers) {
        Ok(headers) => headers,
        Err(error) => {
            send_log_and_toast(
                &state.event_sender,
                CoreLogLevel::Error,
                format!("编辑失败：{}", error.user_message()),
            );
            return;
        }
    };
    if output_directory.trim().is_empty() {
        send_log_and_toast(
            &state.event_sender,
            CoreLogLevel::Error,
            "编辑失败：保存路径不能为空".to_string(),
        );
        return;
    }
    let output_directory = PathBuf::from(output_directory.trim());

    // 先在锁内取出待写回的 manifest，并判断是否需要迁移分片目录。
    //
    // 迁移前主动释放锁是有意为之：下面的复制是同步 IO，持锁执行会长时间占住任务表。
    // 代价是迁移期间任务可能被删除或再次编辑：
    // - 被删除：由下方 TaskGone 分支清理迁移出的目录；
    // - 再次编辑：不会发生。编辑窗口是模态的，迁移完成前 GUI 不会投递第二个 EditTask。
    //   若将来新增并发投递路径（热键、拖拽等），需要先在这里占一个按任务 id 的编辑令牌。
    let (updated, migration) = {
        let Ok(mut tasks) = state.tasks.lock() else {
            return;
        };
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
        let mut updated = runtime.manifest.clone();
        updated.source_url = source_url;
        updated.output_name = sanitize_filename(&output_name);
        updated.output_directory = output_directory;
        updated.request_headers = request_headers;
        // 输出名或保存目录变化后任务目录随之变化，必须迁移已下载的分片。
        let previous_directory = runtime.manifest.task_directory();
        let current_directory = updated.task_directory();
        let migration = (previous_directory != current_directory && previous_directory.is_dir())
            .then_some((previous_directory, current_directory));
        (updated, migration)
    };

    // 迁移完成后要判空撤销，这里先留下目标目录的副本。
    let target_directory = migration.as_ref().map(|(_, to)| to.clone());

    // 迁移是同步 IO，要复制的分片可能有几个 GB。必须放到阻塞线程池并释放任务锁，
    // 否则会占满 tokio worker 让其他下载任务跟着一起卡住。
    if let Some((from, to)) = migration {
        let result = tokio::task::spawn_blocking(move || move_task_directory(&from, &to)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                send_log_and_toast(
                    &state.event_sender,
                    CoreLogLevel::Error,
                    format!("编辑失败：{}", error.user_message()),
                );
                return;
            }
            Err(_) => {
                send_log_and_toast(
                    &state.event_sender,
                    CoreLogLevel::Error,
                    "编辑失败：迁移分片任务被中断".to_string(),
                );
                return;
            }
        }
    }

    // 只有任务在迁移期间被删除才需要清理新目录。删除任务走的是 delete_task，
    // 它清的是 manifest 里记录的旧目录，两条路径不重叠，新目录只能由这里回收。
    // 保存失败时分片已迁到新目录，绝不能撤销，否则会连分片一起删掉。
    if matches!(
        apply_edited_manifest(state, id, updated),
        EditOutcome::TaskGone
    ) {
        // 复制出去的目录成了孤儿，清理掉并提示用户。
        if let Some(to) = target_directory {
            let message = match safe_remove_directory(&to) {
                Ok(()) => "任务已不存在，迁移的分片目录已撤销".to_string(),
                Err(error) => format!("任务已不存在，但清理迁移目录失败：{}", error.user_message()),
            };
            send_log_and_toast(&state.event_sender, CoreLogLevel::Warning, message);
        }
    }
}

/// 编辑写回的结果，决定是否要撤销已完成的目录迁移。
enum EditOutcome {
    /// 已写入并持久化。
    Applied,
    /// 任务在迁移期间已不存在，迁移产生的目录需要撤销。
    TaskGone,
    /// 分片已迁移到位但持久化失败，不能撤销，否则会连分片一起删掉。
    SaveFailed,
}

/// 把编辑后的 manifest 写回任务并推送快照。
fn apply_edited_manifest(state: &ManagerState, id: u64, updated: TaskManifest) -> EditOutcome {
    let Ok(mut tasks) = state.tasks.lock() else {
        return EditOutcome::SaveFailed;
    };
    let Some(runtime) = tasks.get_mut(&id) else {
        return EditOutcome::TaskGone;
    };
    runtime.manifest = updated;
    if let Err(error) = runtime.manifest.save() {
        drop(tasks);
        send_log_and_toast(
            &state.event_sender,
            CoreLogLevel::Error,
            format!("编辑失败：{}", error.user_message()),
        );
        return EditOutcome::SaveFailed;
    }
    runtime.snapshot.source_url = runtime.manifest.source_url.clone();
    runtime.snapshot.output_name = runtime.manifest.output_name.clone();
    runtime.snapshot.output_directory = runtime
        .manifest
        .output_directory
        .to_string_lossy()
        .into_owned();
    runtime.snapshot.request_headers = runtime.manifest.request_headers_json();
    let snapshot = runtime.snapshot.clone();
    drop(tasks);
    let _ = state.event_sender.send(TaskEvent::Snapshot(snapshot));
    let _ = state.event_sender.send(TaskEvent::Log {
        level: CoreLogLevel::Info,
        message: "任务已更新".to_string(),
    });
    EditOutcome::Applied
}

/// 迁移任务临时目录，跨盘符时 rename 会失败，退化为复制后删除。
fn move_task_directory(from: &Path, to: &Path) -> Result<(), CoreError> {
    if from == to {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|_| CoreError::Io("创建任务目录失败".into()))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_directory(from, to)?;
    safe_remove_directory(from)
}

fn copy_directory(from: &Path, to: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(to).map_err(|_| CoreError::Io("创建任务目录失败".into()))?;
    let entries = std::fs::read_dir(from).map_err(|_| CoreError::Io("读取任务目录失败".into()))?;
    for entry in entries {
        let entry = entry.map_err(|_| CoreError::Io("读取任务目录失败".into()))?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_directory(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|_| CoreError::Io("迁移任务分片失败".into()))?;
        }
    }
    Ok(())
}

/// 清除所有已结束的任务（已完成、已失败、已取消）。
fn clear_finished_tasks(state: &ManagerState) {
    let removed: Vec<TaskRuntime> = {
        let Ok(mut tasks) = state.tasks.lock() else {
            return;
        };
        let finished: Vec<u64> = tasks
            .values()
            .filter(|runtime| !runtime.snapshot.status.is_active())
            .map(|runtime| runtime.manifest.id)
            .collect();
        finished.iter().filter_map(|id| tasks.remove(id)).collect()
    };
    if removed.is_empty() {
        return;
    }
    let ids: Vec<u64> = removed.iter().map(|runtime| runtime.manifest.id).collect();
    let mut dismissed = 0;
    for runtime in &removed {
        runtime.cancellation_token.cancel();
        runtime.handle.abort();
        // 未完成的任务被清除后要打标记，否则重启后会被断点续传重新载入。
        if !runtime.manifest.completed {
            let mut manifest = runtime.manifest.clone();
            if let Err(error) = manifest.mark_dismissed() {
                let _ = state.event_sender.send(TaskEvent::Log {
                    level: CoreLogLevel::Warning,
                    message: format!("标记已清除任务失败：{}", error.user_message()),
                });
                continue;
            }
            dismissed += 1;
        }
    }
    let _ = state.event_sender.send(TaskEvent::Log {
        level: CoreLogLevel::Info,
        message: format!(
            "已清除 {} 个已结束的任务，其中 {} 个未完成任务不再自动续传",
            removed.len(),
            dismissed
        ),
    });
    let _ = state.event_sender.send(TaskEvent::TasksRemoved { ids });
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
        if manifest.completed || manifest.dismissed {
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
        request_headers: manifest.request_headers_json(),
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
