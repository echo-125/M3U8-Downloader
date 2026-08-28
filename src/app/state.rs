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
    pub selected_task_id: Option<u64>,
    pub toast: Option<Toast>,
    pub show_exit_confirmation: bool,
    pub edit_task: Option<EditTask>,
    pub allow_exit: bool,
    pub ffmpeg_status: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        let config_path = default_config_path();
        let (settings, warning) = Settings::load_or_default(Some(&config_path));
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
        if let Some(warning) = warning {
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
            selected_task_id: None,
            toast: None,
            show_exit_confirmation: false,
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
                TaskEvent::Log { level, message } => {
                    let level = match level {
                        CoreLogLevel::Info => LogLevel::Info,
                        CoreLogLevel::Warning => LogLevel::Warning,
                        CoreLogLevel::Error => LogLevel::Error,
                    };
                    self.logs.push(level, message);
                }
                TaskEvent::Toast { message, error } => {
                    self.toast = Some(Toast {
                        message,
                        error,
                        expires_at: Instant::now() + std::time::Duration::from_millis(3500),
                    });
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
                                self.logs.push_error(message);
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
                                self.toast = Some(Toast {
                                    message: "手动合并完成".to_string(),
                                    error: false,
                                    expires_at: Instant::now()
                                        + std::time::Duration::from_millis(3500),
                                });
                            }
                            Err(message) => {
                                self.logs.push_error(format!("手动合并失败：{message}"));
                                self.toast = Some(Toast {
                                    message: format!("手动合并失败：{message}"),
                                    error: true,
                                    expires_at: Instant::now()
                                        + std::time::Duration::from_millis(3500),
                                });
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
            self.logs.push_error(format!("设置保存失败：{error}"));
            return;
        }
        if let Err(error) = settings.save(Some(&self.config_path)) {
            self.logs.push_error(format!("设置保存失败：{error}"));
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
            self.logs
                .push_error("任务添加失败：M3U8 链接必须是有效的 HTTP 或 HTTPS 地址");
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
        let output_directory = self.output_directory(&self.batch_path);
        let mut valid = 0;
        let mut invalid = 0;
        for line in self.batch_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('|').map(str::trim).collect();
            if parts.is_empty() || !is_valid_http_url(parts[0]) {
                invalid += 1;
                continue;
            }
            let source_url = parts[0].to_string();
            let output_name = if parts.len() >= 2 && !parts[1].is_empty() {
                sanitize_filename(parts[1])
            } else {
                derive_output_name(&source_url)
            };
            let request_headers = if parts.len() >= 3 {
                parts[2].to_string()
            } else {
                String::new()
            };
            self.manager.send(TaskCommand::Add(NewTask {
                source_url,
                output_name,
                output_directory: output_directory.clone(),
                max_workers: self.settings.max_workers,
                request_headers,
            }));
            valid += 1;
        }
        if valid > 0 {
            self.logs
                .push_info(format!("批量添加完成：成功 {valid} 个"));
            self.batch_text.clear();
        }
        if invalid > 0 {
            self.logs
                .push_error(format!("批量添加有 {invalid} 行无效，已跳过"));
        }
    }

    pub fn scan_manual_folder(&mut self) {
        let folder = PathBuf::from(self.manual_folder.trim());
        if !folder.is_dir() {
            self.logs.push_error("扫描失败：合并文件夹不存在");
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
            self.logs.push_error("合并失败：文件夹不存在");
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

    pub fn retry_task(&mut self, id: u64) {
        self.manager.send(TaskCommand::Retry(id));
    }

    pub fn save_edited_task(&mut self) {
        let Some(edit) = self.edit_task.clone() else {
            return;
        };
        if !is_valid_http_url(&edit.source_url) {
            self.logs
                .push_error("编辑失败：链接必须是有效的 HTTP 或 HTTPS 地址");
            return;
        }
        if edit.output_name.trim().is_empty() {
            self.logs.push_error("编辑失败：文件名不能为空");
            return;
        }
        self.manager.send(TaskCommand::EditTask {
            id: edit.id,
            source_url: edit.source_url.trim().to_string(),
            output_name: edit.output_name.trim().to_string(),
        });
        self.edit_task = None;
    }

    pub fn delete_task(&mut self, id: u64) {
        self.manager.send(TaskCommand::Delete(id));
        self.tasks.retain(|task| task.id != id);
        if self.selected_task_id == Some(id) {
            self.selected_task_id = None;
        }
    }

    pub fn clear_completed_tasks(&mut self) {
        self.manager.send(TaskCommand::ClearCompleted);
        self.tasks
            .retain(|task| task.status != TaskStatus::Completed);
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
            self.logs.push_warning("目录不存在，无法打开");
            return;
        }
        if Command::new("explorer.exe").arg(&path).spawn().is_err() {
            self.logs.push_error("打开目录失败");
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
}
