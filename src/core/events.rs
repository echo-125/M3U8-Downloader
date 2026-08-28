use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::merge::MergeScanResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Waiting,
    Downloading,
    Canceling,
    Completed,
    Failed,
    Canceled,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "等待中",
            Self::Downloading => "下载中",
            Self::Canceling => "取消中",
            Self::Completed => "已完成",
            Self::Failed => "已失败",
            Self::Canceled => "已取消",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Waiting | Self::Downloading | Self::Canceling)
    }

    /// 可以开始或重试的状态。
    /// 已完成任务不在其中：核心启动已完成任务会重置 manifest，导致重新下载并覆盖已有成品。
    pub fn is_startable(self) -> bool {
        matches!(self, Self::Waiting | Self::Failed | Self::Canceled)
    }

    /// 可以取消的状态。取消中重复下发取消命令是幂等的，因此一并按可取消处理。
    pub fn is_cancelable(self) -> bool {
        self.is_active()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: u64,
    pub source_url: String,
    pub output_name: String,
    pub output_directory: String,
    pub request_headers: String,
    pub status: TaskStatus,
    pub completed_segments: usize,
    pub total_segments: usize,
    pub progress: f32,
    pub speed_bytes_per_second: u64,
    pub estimated_seconds_remaining: u64,
    pub detail: String,
    pub output_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreLogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub source_url: String,
    pub output_name: String,
    pub output_directory: PathBuf,
    pub max_workers: usize,
    pub request_headers: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskCommand {
    Add(NewTask),
    Start(u64),
    StartAll,
    Cancel(u64),
    Retry(u64),
    Delete(u64),
    EditTask {
        id: u64,
        source_url: String,
        output_name: String,
        output_directory: String,
        request_headers: String,
    },
    ClearFinished,
    ResumeTasks(Vec<PathBuf>),
    UpdateSettings(crate::config::Settings),
    DetectFfmpeg,
    ScanMergeFolder {
        request_id: u64,
        folder: PathBuf,
    },
    MergeFolder {
        request_id: u64,
        folder: PathBuf,
        output_name: String,
        convert_to_mp4: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskEvent {
    Snapshot(TaskSnapshot),
    /// 核心已删除任务，界面据此移除对应行，避免界面先移除而核心删除失败造成状态不一致。
    TasksRemoved {
        ids: Vec<u64>,
    },
    Log {
        level: CoreLogLevel,
        message: String,
    },
    Toast {
        message: String,
        error: bool,
    },
    MergeScan {
        request_id: u64,
        result: Result<MergeScanResult, String>,
    },
    MergeFinished {
        request_id: u64,
        result: Result<crate::core::merge::MergeResult, String>,
    },
    FfmpegStatus {
        path: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_statuses_are_correct() {
        assert!(TaskStatus::Waiting.is_active());
        assert!(!TaskStatus::Failed.is_active());
    }

    #[test]
    fn completed_tasks_are_not_startable() {
        // 已完成任务不可重新开始：核心启动已完成任务会重置 manifest，重新下载并覆盖已有成品。
        assert!(TaskStatus::Waiting.is_startable());
        assert!(TaskStatus::Failed.is_startable());
        assert!(TaskStatus::Canceled.is_startable());
        assert!(!TaskStatus::Completed.is_startable());
        assert!(!TaskStatus::Downloading.is_startable());
    }
}
