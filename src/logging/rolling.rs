use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use chrono::Local;

use crate::config::LoggingConfig;

#[derive(Debug)]
struct LogFile {
    directory: PathBuf,
    current: File,
    current_path: PathBuf,
    current_date: String,
    max_size_bytes: u64,
    written_bytes: u64,
    daily_rotation: bool,
}

impl LogFile {
    fn open(directory: PathBuf, config: &LoggingConfig) -> Option<Self> {
        std::fs::create_dir_all(&directory).ok()?;
        let current_date = Local::now().format("%Y-%m-%d").to_string();
        let current_path = directory.join(format!("cat-catch.{current_date}.log"));
        let current = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current_path)
            .ok()?;
        let written_bytes = current
            .metadata()
            .ok()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        Some(Self {
            directory,
            current,
            current_path,
            current_date,
            max_size_bytes: config.max_size_mb * 1024 * 1024,
            written_bytes,
            daily_rotation: config.rotation == "daily",
        })
    }

    fn write_line(&mut self, line: &str) {
        let date = Local::now().format("%Y-%m-%d").to_string();
        let rotate_by_date = self.daily_rotation && date != self.current_date;
        if rotate_by_date || self.written_bytes >= self.max_size_bytes {
            self.rotate(date);
        }
        let bytes = line.as_bytes();
        if self.current.write_all(bytes).is_ok() && self.current.flush().is_ok() {
            self.written_bytes += bytes.len() as u64;
        }
    }

    fn rotate(&mut self, date: String) {
        if date != self.current_date {
            self.current_date = date.clone();
            self.current_path = self.directory.join(format!("cat-catch.{date}.log"));
            self.written_bytes = 0;
        } else {
            let mut index = 1;
            let mut path = self.directory.join(format!("cat-catch.{date}.{index}.log"));
            while path.exists() {
                index += 1;
                path = self.directory.join(format!("cat-catch.{date}.{index}.log"));
            }
            self.current_path = path;
            self.written_bytes = 0;
        }
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.current_path)
        {
            self.current = file;
        }
    }
}

#[derive(Clone)]
struct LogWriter {
    file: Arc<Mutex<LogFile>>,
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut file) = self.file.lock() {
            file.write_line(&String::from_utf8_lossy(buf));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct LogWriterMaker {
    file: Arc<Mutex<LogFile>>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriterMaker {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            file: self.file.clone(),
        }
    }
}

pub struct LoggingGuard {
    _file: Option<Arc<Mutex<LogFile>>>,
}

pub fn init(config: &LoggingConfig) -> LoggingGuard {
    let file = if config.file_enabled {
        LogFile::open(log_directory(), config).map(|file| Arc::new(Mutex::new(file)))
    } else {
        None
    };

    match &file {
        Some(file) => {
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_target(false)
                .with_writer(LogWriterMaker { file: file.clone() })
                .try_init();
        }
        None => {
            let _ = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_target(false)
                .try_init();
        }
    }

    LoggingGuard { _file: file }
}

fn log_directory() -> PathBuf {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|path| path.to_path_buf()));
    if let Some(directory) = executable_directory {
        let logs = directory.join("logs");
        if std::fs::create_dir_all(&logs).is_ok() && logs.is_dir() {
            return logs;
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cat-catch-assistant")
        .join("logs")
}
