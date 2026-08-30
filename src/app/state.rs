use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use url::Url;

use crate::{
    config::{default_config_path, ProxyScheme, Settings, ThemeKind},
    core::{
        events::{CoreLogLevel, NewTask, TaskCommand, TaskEvent, TaskSnapshot, TaskStatus},
        manager::TaskManager,
        merge::{sanitize_filename, MergeScanResult},
        task::{TaskRegistry, TASK_REGISTRY_FILE_NAME},
    },
    ffmpeg::FfmpegInfo,
    logging::{LogBuffer, LogLevel},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationTab {
    Single,
    Batch,
    ManualMerge,
}

/// 双击任务行时执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// 开始下载。
    Start,
    /// 打开任务所在目录。
    OpenDirectory,
}

/// 双击是低意图动作（常被当成「看看这行是什么」），因此不承担删除分片这类不可逆后果。
/// 下载中 / 取消中返回 None：这两个状态没有既安全又有意义的双击动作，
/// 静默无反应好过误删已下载的分片——取消入口保留在工具栏与右键菜单。
pub fn double_click_action(status: TaskStatus) -> Option<RowAction> {
    if status.is_startable() {
        Some(RowAction::Start)
    } else if status == TaskStatus::Completed {
        Some(RowAction::OpenDirectory)
    } else {
        None
    }
}

#[derive(Debug)]
pub struct Toast {
    pub message: String,
    pub error: bool,
    /// 自动消失的时刻。错误提示取 `None` 常驻：错误信息常常要读好几行
    /// （比如批量粘贴的逐行报错），几秒钟根本看不完，交给用户手动关。
    pub expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct EditTask {
    pub id: u64,
    pub source_url: String,
    pub output_name: String,
    pub output_directory: String,
    pub request_headers: String,
}

pub struct AppState {
    pub manager: TaskManager,
    pub settings: Settings,
    /// 打开设置窗口时的配置快照，用于关闭窗口时丢弃未保存的修改。
    pub settings_before_edit: Option<Settings>,
    pub config_path: PathBuf,
    pub creation_tab: CreationTab,
    pub single_url: String,
    pub single_path: String,
    pub single_name: String,
    pub single_workers: usize,
    pub single_headers: String,
    pub batch_text: String,
    pub batch_path: String,
    pub manual_folder: String,
    pub manual_output_name: String,
    pub manual_convert_to_mp4: bool,
    pub manual_scan: Option<MergeScanResult>,
    /// 上次扫描成功时对应的文件夹路径，用来判断已展示的扫描结果是否还属于当前路径。
    pub manual_scanned_folder: String,
    pub manual_request_id: u64,
    pub tasks: Vec<TaskSnapshot>,
    pub logs: LogBuffer,
    pub settings_open: bool,
    /// 任务列表的多选集合。用 HashSet 而不是 Vec：列表渲染时每一行都要判断
    /// 自己是否被勾选，Vec 的 contains 是 O(n)，整张列表就成了 O(n²)。
    /// 代价是迭代顺序不确定——下发命令不依赖顺序，无所谓。
    pub selected_task_ids: HashSet<u64>,
    pub toast: Option<Toast>,
    pub show_exit_confirmation: bool,
    /// 弹出退出确认时的进行中任务数快照，任务在确认期间结束时文案不会跳到 0。
    pub exit_confirmation_count: usize,
    /// 清空任务列表前的二次确认弹窗。
    pub show_clear_confirmation: bool,
    /// 关闭设置窗口时，若有未保存的修改则弹窗确认是否放弃。
    pub show_discard_settings_confirmation: bool,
    /// 删除任务前的二次确认弹窗。
    pub show_delete_confirmation: bool,
    /// 待用户确认删除的任务 id，真正下发命令要等确认弹窗里点下去。
    pub pending_delete_ids: Vec<u64>,
    pub edit_task: Option<EditTask>,
    pub allow_exit: bool,
    pub ffmpeg_status: Option<FfmpegInfo>,
}

impl AppState {
    /// `settings` 与 `load_warning` 由入口统一加载后传入，避免重复读取配置文件。
    pub fn new(settings: Settings, load_warning: Option<String>) -> Self {
        let config_path = default_config_path();
        let download_path = settings.normalized_download_path();
        let task_registry_path = config_path.with_file_name(TASK_REGISTRY_FILE_NAME);
        let registry_warning;
        let mut resume_directories = match TaskRegistry::load(&task_registry_path) {
            Ok(registry) => {
                registry_warning = None;
                registry.directories
            }
            Err(error) => {
                registry_warning = Some(format!("任务注册表读取失败：{}", error.user_message()));
                Vec::new()
            }
        };
        if !resume_directories.contains(&download_path) {
            resume_directories.push(download_path);
        }
        let default_workers = settings.max_workers;
        let manager = TaskManager::new(settings.clone(), task_registry_path);
        manager.resume_tasks(resume_directories);

        let mut logs = LogBuffer::default();
        logs.push_info("日志系统已就绪");
        if let Some(warning) = load_warning {
            logs.push_warning(warning);
        }
        if let Some(warning) = registry_warning {
            logs.push_warning(warning);
        }

        let state = Self {
            manager,
            settings,
            settings_before_edit: None,
            config_path,
            creation_tab: CreationTab::Single,
            single_url: String::new(),
            single_path: String::new(),
            single_name: String::new(),
            single_workers: default_workers,
            single_headers: String::new(),
            batch_text: String::new(),
            batch_path: String::new(),
            manual_folder: String::new(),
            manual_output_name: String::new(),
            manual_convert_to_mp4: true,
            manual_scan: None,
            manual_scanned_folder: String::new(),
            manual_request_id: 0,
            tasks: Vec::new(),
            logs,
            settings_open: false,
            selected_task_ids: HashSet::new(),
            toast: None,
            show_exit_confirmation: false,
            exit_confirmation_count: 0,
            show_clear_confirmation: false,
            show_discard_settings_confirmation: false,
            show_delete_confirmation: false,
            pending_delete_ids: Vec::new(),
            edit_task: None,
            allow_exit: false,
            ffmpeg_status: None,
        };
        state.manager.send(TaskCommand::DetectFfmpeg);
        state
    }

    pub fn process_events(&mut self) {
        while let Some(event) = self.manager.try_recv_event() {
            match event {
                TaskEvent::Snapshot(snapshot) => insert_sorted(&mut self.tasks, snapshot),
                TaskEvent::TasksRemoved { ids } => {
                    self.tasks.retain(|task| !ids.contains(&task.id));
                    self.selected_task_ids.retain(|id| !ids.contains(id));
                    // 待确认删除的任务若已被别的操作移除，确认时就不必再下发；
                    // 全部被移走时连弹窗一起收掉，避免弹窗指向空集合。
                    self.pending_delete_ids.retain(|id| !ids.contains(id));
                    if self.pending_delete_ids.is_empty() {
                        self.show_delete_confirmation = false;
                    }
                    if self
                        .edit_task
                        .as_ref()
                        .is_some_and(|edit| ids.contains(&edit.id))
                    {
                        self.edit_task = None;
                    }
                }
                TaskEvent::Log { level, message } => {
                    let level = match level {
                        CoreLogLevel::Info => LogLevel::Info,
                        CoreLogLevel::Warning => LogLevel::Warning,
                        CoreLogLevel::Error => LogLevel::Error,
                    };
                    // 单向桥接到文件日志：核心只走事件通道，若这里不转写，
                    // 任务失败原因、合并结果这类信息就只留在界面日志里，文件日志查不到。
                    //
                    // 注意 tracing 的 writer 内部持有 LogFile 的互斥锁且不可重入，
                    // 因此 process_events 以及它调用的 logs 写入路径，
                    // 都不得再触发任何 tracing 调用，否则会自己锁死自己。
                    match level {
                        LogLevel::Info => tracing::info!("{message}"),
                        LogLevel::Warning => tracing::warn!("{message}"),
                        LogLevel::Error => tracing::error!("{message}"),
                    }
                    self.logs.push(level, message);
                }
                TaskEvent::Toast { message, error } => {
                    self.show_toast(message, error);
                }
                TaskEvent::MergeScan { request_id, result } => {
                    if request_id == self.manual_request_id {
                        match result {
                            Ok(scan) => {
                                self.logs.push_info(format!(
                                    "扫描完成：TS {} 个，fMP4 {} 个{}",
                                    scan.ts_segments.len(),
                                    scan.fmp4_segments.len(),
                                    if scan.initialization.is_some() {
                                        "，已找到初始化段"
                                    } else {
                                        ""
                                    }
                                ));
                                self.manual_scan = Some(scan);
                                // 记下这次扫描对应的路径：路径一改，界面上这份结果就作废。
                                self.manual_scanned_folder = self.manual_folder.trim().to_string();
                            }
                            Err(message) => {
                                self.manual_scan = None;
                                self.manual_scanned_folder.clear();
                                self.notify_error(message);
                            }
                        }
                    }
                }
                TaskEvent::MergeFinished { request_id, result } => {
                    if request_id == self.manual_request_id {
                        match result {
                            Ok(result) => {
                                self.logs.push_info(format!(
                                    "合并完成：{}",
                                    result.output_path.to_string_lossy()
                                ));
                                self.show_toast("手动合并完成", false);
                            }
                            Err(message) => {
                                self.logs.push_error(format!("手动合并失败：{message}"));
                                self.show_toast(format!("手动合并失败：{message}"), true);
                            }
                        }
                    }
                }
                TaskEvent::FfmpegStatus { info } => match info {
                    Some(info) => {
                        self.ffmpeg_status = Some(info);
                        self.logs.push_info("ffmpeg 检测成功");
                    }
                    None => {
                        self.ffmpeg_status = None;
                        self.logs
                            .push_warning("未检测到可用 ffmpeg，TS 任务将保留 TS 输出");
                    }
                },
            }
        }
    }

    pub fn save_settings(&mut self) -> bool {
        let mut settings = self.settings.clone();
        if let Err(error) = settings.validate() {
            self.notify_error(format!("设置保存失败：{error}"));
            return false;
        }
        if let Err(error) = settings.save(Some(&self.config_path)) {
            self.notify_error(format!("设置保存失败：{error}"));
            return false;
        }
        self.settings = settings;
        self.settings_before_edit = Some(self.settings.clone());
        self.manager
            .send(TaskCommand::UpdateSettings(self.settings.clone()));
        self.manager.send(TaskCommand::DetectFfmpeg);
        self.logs.push_info("设置已保存");
        // 保存后给出明确反馈，避免用户以为按钮没反应。
        self.show_toast("设置已保存", false);
        true
    }

    pub fn reset_settings(&mut self) {
        self.settings = Settings::default();
        self.logs.push_info("已恢复默认设置，请点击保存后生效");
    }

    /// 设置窗口里是否存在尚未保存的修改。
    pub fn is_settings_dirty(&self) -> bool {
        is_settings_dirty(self.settings_before_edit.as_ref(), &self.settings)
    }

    /// 放弃未保存的修改：还原到打开窗口时的快照并关闭窗口。
    pub fn discard_settings_edit(&mut self) {
        if let Some(backup) = self.settings_before_edit.take() {
            self.settings = backup;
        }
        self.show_discard_settings_confirmation = false;
        self.settings_open = false;
    }

    pub fn toggle_theme(&mut self) {
        self.settings.appearance.theme = match self.settings.appearance.theme {
            ThemeKind::Light => ThemeKind::Dark,
            ThemeKind::Dark => ThemeKind::Light,
        };
        if let Err(error) = self.settings.save(Some(&self.config_path)) {
            self.logs.push_error(format!("主题保存失败：{error}"));
        }
        self.logs.push_info(format!(
            "已切换到{}主题",
            self.settings.appearance.theme.label()
        ));
    }

    pub fn add_single_task(&mut self) {
        let url = self.single_url.trim().to_string();
        if !is_valid_http_url(&url) {
            self.notify_error("任务添加失败：M3U8 链接必须是有效的 HTTP 或 HTTPS 地址");
            return;
        }
        let output_directory = self.output_directory(&self.single_path);
        let output_name = if self.single_name.trim().is_empty() {
            derive_output_name(&url)
        } else {
            sanitize_filename(self.single_name.trim())
        };
        self.manager.send(TaskCommand::Add(NewTask {
            source_url: url,
            output_name,
            output_directory,
            max_workers: self.single_workers.clamp(1, 64),
            request_headers: self.single_headers.trim().to_string(),
            auto_start: true,
        }));
        self.single_url.clear();
        self.single_name.clear();
    }

    pub fn add_batch_tasks(&mut self) {
        let text = self.batch_text.clone();
        let output_directory = self.output_directory(&self.batch_path);
        let max_workers = self.settings.max_workers;
        let (valid, errors) = self.add_tasks_from_text(&text, output_directory, max_workers, true);
        if valid > 0 {
            self.logs
                .push_info(format!("批量添加完成：成功 {valid} 个"));
            self.batch_text.clear();
        }
        self.report_invalid_lines(&errors);
    }

    /// 从剪贴板读取多行内容并直接添加，跳过手动粘贴到输入框的步骤。
    ///
    /// 失败提示一律只弹提示不写日志：剪贴板里的内容是用户输入，
    /// 格式不合预期属于输入问题，不该占用记录运行异常的日志。
    pub fn paste_and_add_tasks(&mut self) {
        let Some(text) = clipboard_text() else {
            self.show_toast("粘贴添加失败：无法读取剪贴板", true);
            return;
        };
        if text.trim().is_empty() {
            self.show_toast("粘贴添加失败：剪贴板为空", true);
            return;
        }
        // 按钮在单个任务页，因此沿用该页的保存路径与线程数。
        // 粘贴添加不自动开始：批量内容可能包含用户想先检查的链接，保持「等待中」。
        let output_directory = self.output_directory(&self.single_path);
        let max_workers = self.single_workers.clamp(1, 64);
        let (valid, errors) = self.add_tasks_from_text(&text, output_directory, max_workers, false);
        if valid == 0 {
            self.show_toast(
                "粘贴添加失败：剪贴板中没有符合格式的内容\n格式为 链接|文件名 或 链接|文件名|请求头JSON",
                true,
            );
            self.report_invalid_lines(&errors);
            return;
        }
        self.logs
            .push_info(format!("粘贴添加完成：成功 {valid} 个"));
        self.report_invalid_lines(&errors);
    }

    /// 逐行解析 `链接|文件名|请求头JSON` 并下发添加命令。
    /// 返回成功数与逐行的失败原因（已带行号），便于提示用户哪一行格式不对。
    fn add_tasks_from_text(
        &mut self,
        text: &str,
        output_directory: PathBuf,
        max_workers: usize,
        auto_start: bool,
    ) -> (usize, Vec<String>) {
        let mut valid = 0;
        let mut errors = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (source_url, output_name, request_headers) = match parse_task_line(line) {
                Ok(parsed) => parsed,
                Err(reason) => {
                    errors.push(format!("第 {} 行：{reason}", index + 1));
                    continue;
                }
            };
            // 没有写出文件名时按链接推导，保证任务名不为空。
            let output_name = output_name.unwrap_or_else(|| derive_output_name(&source_url));
            self.manager.send(TaskCommand::Add(NewTask {
                source_url,
                output_name,
                output_directory: output_directory.clone(),
                max_workers,
                request_headers,
                auto_start,
            }));
            valid += 1;
        }
        (valid, errors)
    }

    /// 无效行只在界面提示，不写日志：这是粘贴内容的格式问题，不是程序运行异常，
    /// 记进日志会淹没真正的下载错误。
    fn report_invalid_lines(&mut self, errors: &[String]) {
        if errors.is_empty() {
            return;
        }
        let mut message = format!("有 {} 行格式不正确，已跳过", errors.len());
        let shown = errors.len().min(3);
        message.push_str(&format!("：\n{}", errors[..shown].join("\n")));
        if errors.len() > shown {
            message.push_str(&format!("\n……另有 {} 行未显示", errors.len() - shown));
        }
        self.show_toast(message, true);
    }

    pub fn scan_manual_folder(&mut self) {
        let folder = PathBuf::from(self.manual_folder.trim());
        if !folder.is_dir() {
            self.notify_error("扫描失败：合并文件夹不存在");
            return;
        }
        self.manual_request_id = self.manager.next_request_id();
        self.manager.send(TaskCommand::ScanMergeFolder {
            request_id: self.manual_request_id,
            folder,
        });
        self.logs.push_info("正在扫描合并文件夹");
    }

    /// 文件夹路径改了就作废已展示的扫描结果。留着旧结果会让「开始合并」仍可点击，
    /// 点下去却是拿上一次扫描到的分片去合并。
    pub fn invalidate_manual_scan_if_path_changed(&mut self) {
        if self.manual_scan.is_some() && self.manual_scanned_folder != self.manual_folder.trim() {
            self.manual_scan = None;
        }
    }

    pub fn start_manual_merge(&mut self) {
        let folder = PathBuf::from(self.manual_folder.trim());
        if !folder.is_dir() {
            self.notify_error("合并失败：文件夹不存在");
            return;
        }
        // 兜底：界面已按路径变化作废过期的扫描结果，这里再挡一次，
        // 避免同一帧内刚改完路径就点合并。
        if self.manual_scanned_folder != self.manual_folder.trim() {
            self.notify_error("合并失败：请先扫描当前文件夹");
            return;
        }
        let output_name = if self.manual_output_name.trim().is_empty() {
            "manual_merge".to_string()
        } else {
            sanitize_filename(self.manual_output_name.trim())
        };
        self.manual_request_id = self.manager.next_request_id();
        self.manager.send(TaskCommand::MergeFolder {
            request_id: self.manual_request_id,
            folder,
            output_name,
            convert_to_mp4: self.manual_convert_to_mp4,
        });
        self.logs.push_info("正在合并分片");
    }

    pub fn active_task_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.status.is_active())
            .count()
    }

    pub fn start_task(&mut self, id: u64) {
        self.manager.send(TaskCommand::Start(id));
    }

    pub fn start_all_tasks(&mut self) {
        self.manager.send(TaskCommand::StartAll);
    }

    /// 点击行切换勾选状态，作为批量操作的选中集合。
    pub fn toggle_check(&mut self, id: u64) {
        if !self.selected_task_ids.remove(&id) {
            self.selected_task_ids.insert(id);
        }
    }

    pub fn select_all_tasks(&mut self) {
        self.selected_task_ids = self.tasks.iter().map(|task| task.id).collect();
    }

    /// 取消全部勾选。
    pub fn clear_checks(&mut self) {
        self.selected_task_ids.clear();
    }

    /// 选中任务中状态满足条件的 id。
    /// 顺序由 HashSet 决定、不保证稳定，调用方只用来下发命令，不应依赖顺序。
    pub fn selected_ids_where(&self, predicate: fn(TaskStatus) -> bool) -> Vec<u64> {
        ids_where(&self.selected_task_ids, &self.tasks, predicate)
    }

    /// 开始给定任务中处于可开始状态的那些。
    /// 工具栏传当前勾选集合，右键菜单传 `menu_target_ids` 算出的目标集合。
    pub fn start_tasks(&mut self, ids: &[u64]) {
        let targets = ids_where(ids, &self.tasks, TaskStatus::is_startable);
        if targets.is_empty() {
            return;
        }
        for id in &targets {
            self.manager.send(TaskCommand::Start(*id));
        }
        // 批量操作动的是一堆任务，不提示的话用户无法确认自己到底点中了几条。
        self.show_toast(format!("已开始 {} 个任务", targets.len()), false);
    }

    /// 取消任务：停止下载、删除已下载分片、恢复「等待中」，不保留断点续传。
    pub fn reset_tasks(&mut self, ids: &[u64]) {
        let targets = ids_where(ids, &self.tasks, TaskStatus::is_cancelable);
        if targets.is_empty() {
            return;
        }
        self.manager.send(TaskCommand::Reset(targets.clone()));
        self.show_toast(format!("已取消 {} 个任务", targets.len()), false);
    }

    pub fn retry_tasks(&mut self, ids: &[u64]) {
        let targets = ids_where(ids, &self.tasks, TaskStatus::is_startable);
        if targets.is_empty() {
            return;
        }
        for id in &targets {
            self.manager.send(TaskCommand::Retry(*id));
        }
        self.show_toast(format!("已重试 {} 个任务", targets.len()), false);
    }

    /// 只下发删除命令，界面行的移除等核心 `TasksRemoved` 事件确认后再进行。
    ///
    /// 删除会中断进行中的下载、清掉任务的临时分片目录且不可恢复；
    /// `output_directory` 下已合并的成品文件不受影响。
    pub fn delete_tasks(&mut self, ids: &[u64]) {
        for id in ids {
            self.manager.send(TaskCommand::Delete(*id));
        }
    }

    pub fn start_selected_tasks(&mut self) {
        let ids = self.selected_ids_where(TaskStatus::is_startable);
        self.start_tasks(&ids);
    }

    /// 取消勾选任务：停止下载、删除已下载分片、恢复「等待中」，不保留断点续传。
    pub fn cancel_selected_tasks(&mut self) {
        let ids = self.selected_ids_where(TaskStatus::is_cancelable);
        self.reset_tasks(&ids);
    }

    pub fn retry_selected_tasks(&mut self) {
        let ids = self.selected_ids_where(TaskStatus::is_startable);
        self.retry_tasks(&ids);
    }

    /// 登记待删除目标并弹出确认，真正下发命令要等用户在弹窗里点下去。
    ///
    /// 删除会中断进行中的下载、清掉任务的临时分片且不可恢复，而行右键是轻动作，
    /// 误触代价过高，所以这里不直发命令。
    pub fn request_delete_confirmation(&mut self, ids: Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        self.pending_delete_ids = ids;
        self.show_delete_confirmation = true;
    }

    pub fn confirm_delete_pending(&mut self) {
        let ids = std::mem::take(&mut self.pending_delete_ids);
        if !ids.is_empty() {
            self.delete_tasks(&ids);
            self.show_toast(format!("已删除 {} 个任务", ids.len()), false);
        }
        self.show_delete_confirmation = false;
    }

    pub fn cancel_delete_pending(&mut self) {
        self.pending_delete_ids.clear();
        self.show_delete_confirmation = false;
    }

    /// 待删除任务里还在进行的数量：确认文案必须点明会中断下载并清掉已下载的分片。
    pub fn pending_delete_active_count(&self) -> usize {
        ids_where(&self.pending_delete_ids, &self.tasks, TaskStatus::is_active).len()
    }

    /// 工具栏「删除」：无视勾选，移除所有已完成与已失败的任务。
    ///
    /// 会清掉这些任务的临时分片目录，`output_directory` 下的成品文件不受影响；
    /// 未完成任务被打上标记，重启后不再续传。要删除指定任务请用右键菜单，
    /// 那条路径走 `delete_tasks` 并带二次确认。
    pub fn remove_finished_tasks(&mut self) {
        self.manager.send(TaskCommand::RemoveFinished);
    }

    pub fn save_edited_task(&mut self) {
        let Some(edit) = self.edit_task.clone() else {
            return;
        };
        if !is_valid_http_url(&edit.source_url) {
            self.notify_error("编辑失败：链接必须是有效的 HTTP 或 HTTPS 地址");
            return;
        }
        if edit.output_name.trim().is_empty() {
            self.notify_error("编辑失败：文件名不能为空");
            return;
        }
        if edit.output_directory.trim().is_empty() {
            self.notify_error("编辑失败：保存路径不能为空");
            return;
        }
        self.manager.send(TaskCommand::EditTask {
            id: edit.id,
            source_url: edit.source_url.trim().to_string(),
            output_name: edit.output_name.trim().to_string(),
            output_directory: edit.output_directory.trim().to_string(),
            request_headers: edit.request_headers.trim().to_string(),
        });
        self.edit_task = None;
    }

    /// 工具栏「清空」：无视勾选，移除所有已结束的任务（已完成、已失败、已取消）。
    ///
    /// 与 `remove_finished_tasks` 不同，这里不删任何本地文件，只把任务从列表移除。
    pub fn clear_finished_tasks(&mut self) {
        self.manager.send(TaskCommand::ClearFinished);
    }

    /// 从剪贴板粘贴任务信息，支持 `链接|文件名|请求头JSON` 的增强格式。
    pub fn paste_from_clipboard(&mut self) {
        let Some(text) = clipboard_text() else {
            self.show_toast("粘贴失败：无法读取剪贴板", true);
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.show_toast("粘贴失败：剪贴板为空", true);
            return;
        }
        let (source_url, output_name, request_headers) = match parse_task_line(trimmed) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.show_toast(format!("粘贴失败：{reason}"), true);
                return;
            }
        };
        self.single_url = source_url;
        // 只有剪贴板显式带出的字段才覆盖，自动推导的名字不覆盖用户已填内容。
        if let Some(name) = output_name {
            self.single_name = name;
        }
        if !request_headers.is_empty() {
            self.single_headers = request_headers;
        }
        self.logs.push_info("已从剪贴板粘贴链接");
    }

    /// 保存配置但不写日志、不通知核心，用于窗口尺寸这类高频变更。
    pub fn persist_settings(&self) {
        if let Err(error) = self.settings.save(Some(&self.config_path)) {
            tracing::warn!("配置保存失败：{error}");
        }
    }

    pub fn open_task_directory(&mut self, snapshot: &TaskSnapshot) {
        self.open_task_directory_paths(snapshot.output_path.as_deref(), &snapshot.output_directory);
    }

    /// 与 `open_task_directory` 相同，但接收已拷贝的字段：
    /// 任务列表渲染持有任务快照的借用，需要在借用结束后改动 state，
    /// 因此由调用方先把字段拷出来再传值进来。
    pub fn open_task_directory_paths(&mut self, output_path: Option<&str>, output_directory: &str) {
        let path = output_path
            .map(PathBuf::from)
            .map(|path| {
                if path.is_file() {
                    path.parent().map(Path::to_path_buf).unwrap_or(path)
                } else {
                    path
                }
            })
            .unwrap_or_else(|| PathBuf::from(output_directory));
        if !path.is_dir() {
            self.notify_error("目录不存在，无法打开");
            return;
        }
        if Command::new("explorer.exe").arg(&path).spawn().is_err() {
            self.notify_error("打开目录失败");
        }
    }

    /// 打开设置里配置的默认下载目录（设置窗口「打开」按钮）。
    pub fn open_download_directory(&mut self) {
        let path = self.settings.normalized_download_path();
        if !path.is_dir() {
            self.notify_error("下载目录不存在，无法打开");
            return;
        }
        if Command::new("explorer.exe").arg(&path).spawn().is_err() {
            self.notify_error("打开目录失败");
        }
    }

    fn output_directory(&self, input: &str) -> PathBuf {
        if input.trim().is_empty() {
            self.settings.normalized_download_path()
        } else {
            PathBuf::from(input.trim())
        }
    }

    pub fn proxy_scheme_label(&self) -> &'static str {
        match self.settings.proxy.scheme {
            ProxyScheme::Http => "HTTP",
            ProxyScheme::Https => "HTTPS",
            ProxyScheme::Socks5 => "SOCKS5",
        }
    }

    pub fn set_proxy_scheme(&mut self, scheme: ProxyScheme) {
        self.settings.proxy.scheme = scheme;
    }

    /// 弹出一个 Toast，不额外写日志（调用方自行决定日志内容）。
    pub fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        let expires_at = if error {
            None
        } else {
            Some(Instant::now() + std::time::Duration::from_millis(3500))
        };
        self.toast = Some(Toast {
            message: message.into(),
            error,
            expires_at,
        });
    }

    /// 是否存在会自动消失的 Toast。只有这类 Toast 需要界面保持高频重绘等它过期，
    /// 常驻的错误提示不该拖着重绘频率不放。
    pub fn has_expiring_toast(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|toast| toast.expires_at.is_some())
    }

    /// 请求弹出退出确认，同时冻结当时的进行中任务数。
    pub fn request_exit_confirmation(&mut self) {
        self.exit_confirmation_count = self.active_task_count();
        self.show_exit_confirmation = true;
    }

    /// 用户主动操作的失败提示：写错误日志并弹 Toast，确保错误能被注意到。
    pub fn notify_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.logs.push_error(message.clone());
        self.show_toast(message, true);
    }

    pub fn expire_toast(&mut self) {
        let expired = self.toast.as_ref().is_some_and(|toast| {
            toast
                .expires_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        });
        if expired {
            self.toast = None;
        }
    }
}

/// 把快照放进按 id 升序的列表：已存在则替换，否则插到正确位置。
///
/// 替代此前的「push 后整体 sort」——下载中每个任务每 200ms 推一个快照，
/// N 个任务就是每秒 5N 次 O(n log n) 排序，全部浪费在维持同一个有序性上。
pub fn insert_sorted(tasks: &mut Vec<TaskSnapshot>, snapshot: TaskSnapshot) {
    match tasks.binary_search_by_key(&snapshot.id, |task| task.id) {
        Ok(index) => tasks[index] = snapshot,
        Err(index) => tasks.insert(index, snapshot),
    }
}

/// 设置是否被改动过。没有编辑前的快照（窗口未打开）时算作未改动。
///
/// 靠 `PartialEq` 逐字段比较而不是脏标记：改回原值应当算作没改过，
/// 用户不会希望「改了一下又改回来」也弹确认。
pub fn is_settings_dirty(before: Option<&Settings>, current: &Settings) -> bool {
    before.is_some_and(|before| before != current)
}

/// 右键菜单的作用目标。右键未勾选的行时把本行并入已有勾选，而不是替换掉。
///
/// 用户已经勾选的多个任务一旦被静默清空就找不回来了，代价远高于多选一行。
/// 接受任意产生 `&u64` 的迭代器，以便同时用于勾选集合（HashSet）和菜单算出的 Vec；
/// 做成自由函数是为了单测——构造 AppState 会启动 tokio 运行时并读取用户配置。
pub fn menu_target_ids<'a>(
    selected: impl IntoIterator<Item = &'a u64> + Clone,
    row_id: u64,
    row_checked: bool,
) -> Vec<u64> {
    if row_checked || selected.clone().into_iter().any(|id| *id == row_id) {
        return selected.into_iter().copied().collect();
    }
    let mut ids: Vec<u64> = selected.into_iter().copied().collect();
    ids.push(row_id);
    ids
}

/// 按状态过滤任务 id，保持传入顺序；id 已不在列表中的任务会被忽略。
pub fn ids_where<'a>(
    ids: impl IntoIterator<Item = &'a u64>,
    tasks: &[TaskSnapshot],
    predicate: fn(TaskStatus) -> bool,
) -> Vec<u64> {
    ids.into_iter()
        .copied()
        .filter(|id| {
            tasks
                .iter()
                .find(|task| task.id == *id)
                .is_some_and(|task| predicate(task.status))
        })
        .collect()
}

/// 校验请求头文本是合法的 JSON 对象。
///
/// 格式错误时必须拦下：请求头被静默忽略的话下载会失败，而用户很难想到是粘贴内容的问题。
/// 空文本视为未填写请求头，允许通过。
fn validate_header_json(text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("请求头 JSON 格式错误：{error}"))?;
    if !value.is_object() {
        return Err(r#"请求头必须是 JSON 对象，例如 {"Referer":"https://example.com"}"#.into());
    }
    Ok(())
}

/// 解析 `链接|文件名|请求头JSON`，格式不合法时返回具体原因。
///
/// 只按前两个竖线切分：请求头取剩余的全部内容，这样 JSON 内部的 `|` 不会被切断。
/// 链接自身若含 `|` 会被当成分隔符，这是竖线分隔格式的固有歧义，无法可靠区分。
/// 返回（链接，显式给出的文件名，请求头）。文件名未写出时为 None。
fn parse_task_line(line: &str) -> Result<(String, Option<String>, String), String> {
    let mut parts = line.splitn(3, '|').map(str::trim);
    let source_url = parts.next().unwrap_or_default();
    if source_url.is_empty() {
        return Err("链接为空".into());
    }
    if !is_valid_http_url(source_url) {
        return Err("链接不是有效的 HTTP 或 HTTPS 地址".into());
    }
    let output_name = parts
        .next()
        .filter(|name| !name.is_empty())
        .map(sanitize_filename);
    let request_headers = match parts.next() {
        Some(headers) => {
            validate_header_json(headers)?;
            headers.to_string()
        }
        None => String::new(),
    };
    Ok((source_url.to_string(), output_name, request_headers))
}

pub fn is_valid_http_url(value: &str) -> bool {
    Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

/// 单任务表单链接的内联校验提示。
///
/// 为空时放行（留空提交由 `add_single_task` 的校验兜底），非空但不合法时返回原因，
/// 供输入框下方即时提示——用户打字时就能看到问题，而不是点了提交才弹 Toast。
pub fn url_validation_hint(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || is_valid_http_url(url) {
        return None;
    }
    Some("链接必须是有效的 HTTP 或 HTTPS 地址".into())
}

pub fn derive_output_name(url: &str) -> String {
    let name = Url::parse(url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(str::to_string))
        })
        .filter(|segment| !segment.is_empty())
        .unwrap_or_else(|| "video".to_string());
    sanitize_filename(name.trim_end_matches(".m3u8"))
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut clipboard| clipboard.get_text().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_http_url() {
        assert!(is_valid_http_url("https://example.com/a.m3u8"));
        assert!(!is_valid_http_url("ftp://example.com/a.m3u8"));
        assert!(!is_valid_http_url("example.com/a.m3u8"));
    }

    #[test]
    fn derives_safe_output_name() {
        assert_eq!(
            derive_output_name("https://example.com/video/a:b.m3u8"),
            "a_b"
        );
    }

    #[test]
    fn parses_task_line_in_all_forms() {
        let (url, name, headers) =
            parse_task_line(r#"https://a.com/v.m3u8|我的视频|{"Referer":"x"}"#).unwrap();
        assert_eq!(url, "https://a.com/v.m3u8");
        assert_eq!(name.as_deref(), Some("我的视频"));
        assert_eq!(headers, r#"{"Referer":"x"}"#);

        // 省略文件名时返回 None，由调用方决定是否自动推导
        let (_, name, headers) = parse_task_line("https://a.com/path/video.m3u8").unwrap();
        assert_eq!(name, None);
        assert_eq!(headers, "");

        // 文件名是空白时同样视为未给出
        let (_, name, _) = parse_task_line("https://a.com/path/video.m3u8|   ").unwrap();
        assert_eq!(name, None);
    }

    #[test]
    fn accepts_empty_request_headers() {
        // 显式留空的请求头视为未填写，不应报错
        let (_, _, headers) = parse_task_line("https://a.com/v.m3u8|视频|").unwrap();
        assert_eq!(headers, "");
    }

    #[test]
    fn rejects_bad_request_headers() {
        // 非法的 JSON 必须拦下，否则请求头会被静默忽略导致下载失败
        assert!(parse_task_line(r#"https://a.com/v.m3u8|视频|{"Referer":}"#).is_err());
        // 常见的 Python 字典字面量不是合法 JSON
        assert!(parse_task_line("https://a.com/v.m3u8|视频|{'Referer':'x'}").is_err());
        // 必须是对象而不是数组或标量
        assert!(parse_task_line(r#"https://a.com/v.m3u8|视频|["a"]"#).is_err());
    }

    #[test]
    fn rejects_invalid_links() {
        assert!(parse_task_line("not-a-url").is_err());
        assert!(parse_task_line("").is_err());
        assert!(parse_task_line("ftp://a.com/v.m3u8").is_err());
    }

    #[test]
    fn inline_url_hint_matches_submit_validation() {
        // 为空放行：让用户慢慢打字，提交时再兜底
        assert_eq!(url_validation_hint(""), None);
        assert_eq!(url_validation_hint("   "), None);
        // 合法链接不提示
        assert_eq!(url_validation_hint("https://example.com/a.m3u8"), None);
        // 非法时给出与提交校验一致的原因
        assert!(url_validation_hint("example.com/a.m3u8").is_some());
        assert!(url_validation_hint("ftp://a.com/v.m3u8").is_some());
    }

    #[test]
    fn keeps_pipe_inside_request_headers() {
        // 请求头取剩余全部内容，JSON 内部的竖线不会被当成字段分隔符切断
        let (_, name, headers) =
            parse_task_line(r#"https://a.com/v.m3u8|视频|{"UA":"a|b"}"#).unwrap();
        assert_eq!(name.as_deref(), Some("视频"));
        assert_eq!(headers, r#"{"UA":"a|b"}"#);
    }

    #[test]
    fn rejects_extra_fields_instead_of_ignoring_them() {
        // 超过三段的竖线会并入请求头导致 JSON 非法，
        // 必须报错而不是静默丢弃多余字段
        assert!(parse_task_line(r#"https://a.com/v.m3u8|视频|{"UA":"x"}|多余"#).is_err());
    }

    /// 构造最小任务快照，只填判定逻辑关心的字段。
    fn snapshot(id: u64, status: TaskStatus) -> TaskSnapshot {
        TaskSnapshot {
            id,
            source_url: String::new(),
            output_name: format!("task{id}"),
            output_directory: String::new(),
            request_headers: String::new(),
            status,
            completed_segments: 0,
            total_segments: 0,
            progress: 0.0,
            speed_bytes_per_second: 0,
            estimated_seconds_remaining: 0,
            detail: String::new(),
            output_path: None,
        }
    }

    #[test]
    fn menu_target_keeps_existing_selection() {
        // 右键未勾选的行：把本行并入已有勾选，而不是替换掉
        assert_eq!(menu_target_ids(&[1, 2, 3], 7, false), vec![1, 2, 3, 7]);
        // 右键已勾选的行：原样返回全部勾选
        assert_eq!(menu_target_ids(&[1, 2, 3], 2, true), vec![1, 2, 3]);
        // 行已经在勾选集合里时不重复添加
        assert_eq!(menu_target_ids(&[1, 2, 3], 3, false), vec![1, 2, 3]);
        // 无勾选时只作用于本行
        assert_eq!(menu_target_ids(&[], 5, false), vec![5]);
    }

    #[test]
    fn filters_ids_by_status_keeping_order() {
        let tasks = vec![
            snapshot(1, TaskStatus::Waiting),
            snapshot(2, TaskStatus::Downloading),
            snapshot(3, TaskStatus::Failed),
            snapshot(4, TaskStatus::Completed),
        ];

        // 只保留可开始的，且保持传入顺序而不是列表顺序
        assert_eq!(
            ids_where(&[3, 1, 2, 4], &tasks, TaskStatus::is_startable),
            vec![3, 1]
        );
        assert_eq!(
            ids_where(&[1, 2, 4], &tasks, TaskStatus::is_cancelable),
            vec![1, 2]
        );
        // id 对应的任务已被移除时忽略，不把失效 id 传给核心
        assert_eq!(
            ids_where(&[1, 99], &tasks, TaskStatus::is_startable),
            vec![1]
        );
        assert!(ids_where(&[], &tasks, TaskStatus::is_startable).is_empty());
    }

    #[test]
    fn menu_target_merges_into_hash_set_selection() {
        // 勾选集合是 HashSet，迭代顺序不确定，因此按集合比较而不是按序比较
        let selected: HashSet<u64> = [1, 2, 3].into_iter().collect();

        let merged: HashSet<u64> = menu_target_ids(&selected, 7, false).into_iter().collect();
        assert_eq!(merged, [1, 2, 3, 7].into_iter().collect::<HashSet<u64>>());

        // 本行已在集合里时不重复添加
        let unchanged: HashSet<u64> = menu_target_ids(&selected, 2, false).into_iter().collect();
        assert_eq!(unchanged, selected);
    }

    #[test]
    fn inserts_snapshots_in_id_order() {
        let mut tasks = Vec::new();
        for id in [5_u64, 1, 3, 9] {
            insert_sorted(&mut tasks, snapshot(id, TaskStatus::Waiting));
        }
        assert_eq!(
            tasks.iter().map(|task| task.id).collect::<Vec<_>>(),
            vec![1, 3, 5, 9],
            "快照乱序到达也要保持列表按 id 有序"
        );

        // 同一个 id 再来一次是替换而不是追加
        insert_sorted(&mut tasks, snapshot(3, TaskStatus::Downloading));
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[1].id, 3);
        assert_eq!(tasks[1].status, TaskStatus::Downloading);
    }

    #[test]
    fn detects_unsaved_settings_changes() {
        let original = Settings::default();
        // 窗口没打开（没有编辑前快照）时不算脏
        assert!(!is_settings_dirty(None, &original));
        // 与快照一致时不算脏，保存成功后走的正是这条
        assert!(!is_settings_dirty(Some(&original), &original));

        let mut changed = original.clone();
        changed.download_path = "D:\\downloads".to_string();
        assert!(is_settings_dirty(Some(&original), &changed));

        // 改了又改回来不算脏：靠 PartialEq 逐字段比较，不是脏标记
        let mut restored = changed.clone();
        restored.download_path = original.download_path.clone();
        assert!(!is_settings_dirty(Some(&original), &restored));
    }

    #[test]
    fn double_click_never_destroys_progress() {
        // 过去双击下载中的行会重置任务、删掉已下载的分片，这里锁定新行为
        assert_eq!(double_click_action(TaskStatus::Downloading), None);
        assert_eq!(double_click_action(TaskStatus::Canceling), None);

        assert_eq!(
            double_click_action(TaskStatus::Waiting),
            Some(RowAction::Start)
        );
        assert_eq!(
            double_click_action(TaskStatus::Failed),
            Some(RowAction::Start)
        );
        assert_eq!(
            double_click_action(TaskStatus::Canceled),
            Some(RowAction::Start)
        );
        assert_eq!(
            double_click_action(TaskStatus::Completed),
            Some(RowAction::OpenDirectory)
        );
    }
}
