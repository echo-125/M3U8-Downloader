use std::collections::VecDeque;

use super::MAX_GUI_ENTRIES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

impl LogLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "信息",
            Self::Warning => "警告",
            Self::Error => "错误",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    /// 产生时间，格式 HH:MM:SS，用于界面展示。
    pub time: String,
}

#[derive(Debug, Default)]
pub struct LogBuffer {
    entries: VecDeque<LogEntry>,
}

impl LogBuffer {
    pub fn push_info(&mut self, message: impl Into<String>) {
        self.push(LogLevel::Info, message);
    }

    pub fn push(&mut self, level: LogLevel, message: impl Into<String>) {
        if self.entries.len() == MAX_GUI_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(LogEntry {
            level,
            message: message.into(),
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
        });
    }

    pub fn push_warning(&mut self, message: impl Into<String>) {
        self.push(LogLevel::Warning, message);
    }

    pub fn push_error(&mut self, message: impl Into<String>) {
        self.push(LogLevel::Error, message);
    }

    pub fn entries(&self) -> &VecDeque<LogEntry> {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_keeps_only_latest_entries() {
        let mut buffer = LogBuffer::default();
        for index in 0..(MAX_GUI_ENTRIES + 20) {
            buffer.push_info(format!("日志 {index}"));
        }
        assert_eq!(buffer.entries().len(), MAX_GUI_ENTRIES);
        assert_eq!(
            buffer.entries().back().unwrap().message,
            format!("日志 {}", MAX_GUI_ENTRIES + 19)
        );
    }
}
