use std::{
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
    logging::{LogBuffer, LogLevel},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationTab {
    Single,
    Batch,
    ManualMerge,
}

#[derive(Debug)]
pub struct Toast {
    pub message: String,
    pub error: bool,
    pub expires_at: Instant,
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
    pub manual_request_id: u64,
    pub tasks: Vec<TaskSnapshot>,
    pub logs: LogBuffer,
    pub settings_open: bool,
    /// 任务列表支持多选，最后一个被选中的作为编辑等单选操作的默认目标。
    pub selected_task_ids: Vec<u64>,
    pub toast: Option<Toast>,
    pub show_exit_confirmation: bool,
    /// 弹出退出确认时的进行中任务数快照，任务在确认期间结束时文案不会跳到 0。
    pub exit_confirmation_count: usize,
    pub edit_task: Option<EditTask>,
    pub allow_exit: bool,
    pub ffmpeg_status: Option<String>,
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
            manual_request_id: 0,
            tasks: Vec::new(),
            logs,
            settings_open: false,
            selected_task_ids: Vec::new(),
            toast: None,
            show_exit_confirmation: false,
            exit_confirmation_count: 0,
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
                TaskEvent::Snapshot(snapshot) => {
                    if let Some(existing) =
                        self.tasks.iter_mut().find(|task| task.id == snapshot.id)
                    {
                        *existing = snapshot;
                    } else {
                        self.tasks.push(snapshot);
                    }
                    self.tasks.sort_by_key(|task| task.id);
                }
                TaskEvent::TasksRemoved { ids } => {
                    self.tasks.retain(|task| !ids.contains(&task.id));
                    self.selected_task_ids.retain(|id| !ids.contains(id));
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
                            }
                            Err(message) => {
                                self.manual_scan = None;
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
                TaskEvent::FfmpegStatus { path } => match path {
                    Some(path) => {
                        self.ffmpeg_status = Some(path);
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

    pub fn save_settings(&mut self) {
        let mut settings = self.settings.clone();
        if let Err(error) = settings.validate() {
            self.notify_error(format!("设置保存失败：{error}"));
            return;
        }
        if let Err(error) = settings.save(Some(&self.config_path)) {
            self.notify_error(format!("设置保存失败：{error}"));
            return;
        }
        self.settings = settings;
        self.settings_before_edit = Some(self.settings.clone());
        self.manager
            .send(TaskCommand::UpdateSettings(self.settings.clone()));
        self.manager.send(TaskCommand::DetectFfmpeg);
        self.logs.push_info("设置已保存");
    }

    pub fn reset_settings(&mut self) {
        self.settings = Settings::default();
        self.logs.push_info("已恢复默认设置，请点击保存后生效");
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
        }));
        self.single_url.clear();
        self.single_name.clear();
    }

    pub fn add_batch_tasks(&mut self) {
        let text = self.batch_text.clone();
        let output_directory = self.output_directory(&self.batch_path);
        let max_workers = self.settings.max_workers;
        let (valid, errors) = self.add_tasks_from_text(&text, output_directory, max_workers);
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
        let output_directory = self.output_directory(&self.single_path);
        let max_workers = self.single_workers.clamp(1, 64);
        let (valid, errors) = self.add_tasks_from_text(&text, output_directory, max_workers);
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

    pub fn start_manual_merge(&mut self) {
        let folder = PathBuf::from(self.manual_folder.trim());
        if !folder.is_dir() {
            self.notify_error("合并失败：文件夹不存在");
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

    pub fn cancel_task(&mut self, id: u64) {
        self.manager.send(TaskCommand::Cancel(id));
    }

    pub fn is_task_selected(&self, id: u64) -> bool {
        self.selected_task_ids.contains(&id)
    }

    /// 按住 Ctrl 或 Shift 点击为增减选择，否则只选中当前行。
    pub fn select_task(&mut self, id: u64, additive: bool) {
        if !additive {
            self.selected_task_ids = vec![id];
            return;
        }
        match self
            .selected_task_ids
            .iter()
            .position(|selected| *selected == id)
        {
            Some(index) => {
                self.selected_task_ids.remove(index);
            }
            None => self.selected_task_ids.push(id),
        }
    }

    pub fn select_all_tasks(&mut self) {
        self.selected_task_ids = self.tasks.iter().map(|task| task.id).collect();
    }

    /// 选中任务中状态满足条件的 id，保持原选中顺序。
    pub fn selected_ids_where(&self, predicate: fn(TaskStatus) -> bool) -> Vec<u64> {
        self.selected_task_ids
            .iter()
            .copied()
            .filter(|id| self.status_of(*id).is_some_and(predicate))
            .collect()
    }

    pub fn status_of(&self, id: u64) -> Option<TaskStatus> {
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.status)
    }

    pub fn start_selected_tasks(&mut self) {
        for id in self.selected_ids_where(TaskStatus::is_startable) {
            self.manager.send(TaskCommand::Start(id));
        }
    }

    pub fn cancel_selected_tasks(&mut self) {
        for id in self.selected_ids_where(TaskStatus::is_cancelable) {
            self.manager.send(TaskCommand::Cancel(id));
        }
    }

    pub fn retry_selected_tasks(&mut self) {
        for id in self.selected_ids_where(TaskStatus::is_startable) {
            self.manager.send(TaskCommand::Retry(id));
        }
    }

    /// 只下发删除命令，界面行的移除等核心 `TasksRemoved` 事件确认后再进行。
    pub fn delete_selected_tasks(&mut self) {
        for id in self.selected_task_ids.clone() {
            self.manager.send(TaskCommand::Delete(id));
        }
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

    /// 清除所有已结束的任务：已完成、已失败、已取消。
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
        let path = snapshot
            .output_path
            .as_ref()
            .map(PathBuf::from)
            .map(|path| {
                if path.is_file() {
                    path.parent().map(Path::to_path_buf).unwrap_or(path)
                } else {
                    path
                }
            })
            .unwrap_or_else(|| PathBuf::from(&snapshot.output_directory));
        if !path.is_dir() {
            self.notify_error("目录不存在，无法打开");
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
        self.toast = Some(Toast {
            message: message.into(),
            error,
            expires_at: Instant::now() + std::time::Duration::from_millis(3500),
        });
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
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| Instant::now() >= toast.expires_at)
        {
            self.toast = None;
        }
    }
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
}
