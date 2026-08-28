mod app;
mod config;
mod core;
mod ffmpeg;
mod logging;

use crate::app::CatCatchApp;
use crate::config::Settings;

fn main() -> eframe::Result {
    let (settings, warning) = Settings::load_or_default(None);
    let _logging_guard = logging::init(&settings.logging);
    tracing::info!("应用启动");
    if let Some(warning) = warning {
        tracing::warn!("{warning}");
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([820.0, 560.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Cat Catch Assistant",
        options,
        Box::new(|creation_context| Ok(Box::new(CatCatchApp::new(creation_context)))),
    )
}

fn load_icon() -> std::sync::Arc<eframe::egui::IconData> {
    let rgba = crate::app::tray::icon_rgba();
    std::sync::Arc::new(eframe::egui::IconData {
        rgba,
        width: 32,
        height: 32,
    })
}
