// 发布版不挂控制台，避免启动时弹出黑色窗口；debug 构建保留以便看运行日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod core;
mod ffmpeg;
mod logging;

use crate::app::CatCatchApp;
use crate::config::Settings;

fn main() -> eframe::Result {
    let (settings, mut warning) = Settings::load_or_default(None);
    // 日志随下载路径走：收在下载目录的 `.cat-catch-tasks/logs/` 里，下载目录只看到
    // 一个隐藏式前缀目录。目录在启动时确定，运行中改下载路径要重启后才生效。
    let log_directory = settings
        .normalized_download_path()
        .join(crate::core::task::TASK_DIRECTORY_NAME)
        .join("logs");
    let (_logging_guard, logging_warning) = logging::init(&settings.logging, log_directory);
    tracing::info!("应用启动");
    if let Some(message) = &logging_warning {
        // 文件日志打不开时用户没有任何渠道看到 warning：合并进配置警告一起显示。
        warning = match warning {
            Some(existing) => Some(format!("{existing}\n{message}")),
            None => Some(message.clone()),
        };
        tracing::warn!("{message}");
    } else if let Some(message) = &warning {
        tracing::warn!("{message}");
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([
                settings.appearance.window_width,
                settings.appearance.window_height,
            ])
            .with_min_inner_size([820.0, 560.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "M3U8下载器",
        options,
        Box::new(move |creation_context| {
            Ok(Box::new(CatCatchApp::new(
                creation_context,
                settings,
                warning,
            )))
        }),
    )
}

fn load_icon() -> std::sync::Arc<eframe::egui::IconData> {
    let (rgba, width, height) = crate::app::tray::icon_rgba();
    std::sync::Arc::new(eframe::egui::IconData {
        rgba,
        width,
        height,
    })
}
