use std::path::PathBuf;

use crate::config::ThemeKind;
use eframe::egui;

const CJK_FONT_FILES: &[&str] = &[
    "msyh.ttc",
    "msyh.ttf",
    "msyhl.ttc",
    "simhei.ttf",
    "simsun.ttc",
    "Deng.ttf",
];

pub fn switch_label(theme: ThemeKind) -> &'static str {
    match theme {
        ThemeKind::Light => "切换到暗色",
        ThemeKind::Dark => "切换到亮色",
    }
}

pub fn install_fonts(ctx: &egui::Context) -> Option<String> {
    let Some((source, data)) = find_cjk_font() else {
        return Some("未找到系统中文字体，界面中文将无法显示".to_string());
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_owned(), egui::FontData::from_owned(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    tracing::info!("已加载中文字体：{source}");
    None
}

fn find_cjk_font() -> Option<(String, Vec<u8>)> {
    let font_directory = windows_fonts_directory();
    for file in CJK_FONT_FILES {
        let path = font_directory.join(file);
        if let Ok(data) = std::fs::read(&path) {
            return Some((path.to_string_lossy().into_owned(), data));
        }
    }
    None
}

fn windows_fonts_directory() -> PathBuf {
    match std::env::var("WINDIR") {
        Ok(windows) => PathBuf::from(windows).join("Fonts"),
        Err(_) => PathBuf::from(r"C:\Windows\Fonts"),
    }
}

pub fn apply(ctx: &egui::Context, theme: ThemeKind) {
    let visuals = match theme {
        ThemeKind::Light => egui::Visuals::light(),
        ThemeKind::Dark => egui::Visuals::dark(),
    };
    ctx.set_visuals(visuals);
}
