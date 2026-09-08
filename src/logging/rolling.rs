use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
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

/// 初始化日志；返回需要用户知晓的警告（目录回退或完全不可用时）。
///
/// 日志目录由入口按「下载路径 / `.cat-catch-tasks` / logs」构造后传入——目录归属
/// 下载路径是行为约定，但本模块不该知道任务目录的命名规则，只负责在给定目录里开文件。
/// 目录在启动时确定一次，运行中改下载路径不影响已打开的日志文件，下次启动生效。
///
/// 请求目录打不开（网络盘未挂载、U 盘只读等）时回退到 exe 同目录的 `logs/`，
/// 回退成功也返回提示，避免用户以为日志丢了；两次都失败才放弃文件日志。
///
/// 发布版不挂控制台（windows_subsystem），文件日志打不开时 stdout 又是空转——
/// 用户会完全无感知地失去所有日志。这里把警告带出来，由入口把它显示到 GUI 日志面板。
pub fn init(
    config: &LoggingConfig,
    requested_directory: PathBuf,
) -> (LoggingGuard, Option<String>) {
    let fallback_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("logs");

    let mut fell_back = false;
    let file = if config.file_enabled {
        let opened = LogFile::open(requested_directory, config)
            .or_else(|| {
                fell_back = true;
                LogFile::open(fallback_directory, config)
            })
            .map(|file| Arc::new(Mutex::new(file)));
        if opened.is_none() {
            tracing::warn!("文件日志不可用：日志目录创建失败，日志仅保留在内存");
        }
        opened
    } else {
        None
    };

    let warning = if !config.file_enabled || file.is_some() && !fell_back {
        None
    } else if file.is_some() {
        Some("下载路径不可写，日志已记录到程序目录下的 logs 文件夹".to_string())
    } else {
        Some(
            "文件日志不可用：下载路径与程序目录均不可写，运行日志仅保留在本窗口的日志面板"
                .to_string(),
        )
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

    (LoggingGuard { _file: file }, warning)
}
