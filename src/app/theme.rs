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

/// 一套主题用到的配色，集中定义便于整体调整。
struct Palette {
    accent: egui::Color32,
    panel: egui::Color32,
    card: egui::Color32,
    field: egui::Color32,
    faint: egui::Color32,
    widget: egui::Color32,
    widget_hover: egui::Color32,
    border: egui::Color32,
}

impl Palette {
    fn new(theme: ThemeKind) -> Self {
        match theme {
            ThemeKind::Light => Self {
                accent: egui::Color32::from_rgb(37, 118, 214),
                panel: egui::Color32::from_rgb(244, 246, 249),
                card: egui::Color32::WHITE,
                field: egui::Color32::WHITE,
                faint: egui::Color32::from_rgb(240, 243, 247),
                widget: egui::Color32::WHITE,
                widget_hover: egui::Color32::from_rgb(236, 243, 253),
                border: egui::Color32::from_rgb(219, 225, 232),
            },
            ThemeKind::Dark => Self {
                accent: egui::Color32::from_rgb(76, 154, 255),
                panel: egui::Color32::from_rgb(27, 30, 36),
                card: egui::Color32::from_rgb(36, 40, 48),
                field: egui::Color32::from_rgb(21, 24, 29),
                faint: egui::Color32::from_rgb(31, 34, 41),
                widget: egui::Color32::from_rgb(42, 47, 56),
                widget_hover: egui::Color32::from_rgb(50, 56, 66),
                border: egui::Color32::from_rgb(56, 63, 74),
            },
        }
    }
}

/// 按钮上只写目标主题，名称保持简短。
pub fn switch_label(theme: ThemeKind) -> &'static str {
    match theme {
        ThemeKind::Light => "暗色",
        ThemeKind::Dark => "亮色",
    }
}

/// 悬停时补足完整说明，避免只看到「暗色」两个字不清楚是切换还是当前状态。
pub fn switch_hint(theme: ThemeKind) -> &'static str {
    match theme {
        ThemeKind::Light => "切换到暗色主题",
        ThemeKind::Dark => "切换到亮色主题",
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

/// 基于默认明暗视觉派生完整样式：加大字号、统一间距圆角，并注入强调色。
pub fn apply(ctx: &egui::Context, theme: ThemeKind) {
    let palette = Palette::new(theme);
    let mut style = match theme {
        ThemeKind::Light => egui::Style {
            visuals: egui::Visuals::light(),
            ..egui::Style::default()
        },
        ThemeKind::Dark => egui::Style {
            visuals: egui::Visuals::dark(),
            ..egui::Style::default()
        },
    };

    style.text_styles = [
        (egui::TextStyle::Heading, egui::FontId::proportional(20.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(15.0)),
        (egui::TextStyle::Button, egui::FontId::proportional(15.0)),
        (egui::TextStyle::Small, egui::FontId::proportional(12.0)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(13.0)),
    ]
    .into();

    style.spacing.item_spacing = egui::vec2(10.0, 9.0);
    // 输入框 margin 与按钮 padding 的纵向值保持一致，两者高度才会相等。
    style.spacing.button_padding = egui::vec2(12.0, 5.0);
    style.spacing.interact_size = egui::vec2(40.0, 30.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    style.spacing.scroll = egui::style::ScrollStyle::solid();

    let visuals = &mut style.visuals;
    visuals.panel_fill = palette.panel;
    visuals.window_fill = palette.card;
    visuals.extreme_bg_color = palette.field;
    visuals.faint_bg_color = palette.faint;
    visuals.window_rounding = egui::Rounding::same(10.0);
    visuals.menu_rounding = egui::Rounding::same(8.0);
    visuals.selection.bg_fill = palette.accent;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    visuals.hyperlink_color = palette.accent;

    let widgets = &mut visuals.widgets;
    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.rounding = egui::Rounding::same(6.0);
    }
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, palette.border);
    widgets.inactive.bg_fill = palette.widget;
    widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, palette.border);
    widgets.hovered.bg_fill = palette.widget_hover;
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, palette.accent);
    widgets.active.bg_fill = palette.accent;
    widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);

    ctx.set_style(style);
}
