use std::path::PathBuf;

use crate::config::ThemeKind;
use crate::core::events::TaskStatus;
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

/// 强调色：主按钮填充、选中 tab 下划线、超链接、选中行背景都用它。
/// 不要直接读 `visuals.selection.bg_fill`——那只是「文本选中高亮」的背景，
/// 浅色主题下必须是浅色（深色文字才能看清），与强调色语义不同。
pub fn accent_color(dark_mode: bool) -> egui::Color32 {
    if dark_mode {
        egui::Color32::from_rgb(76, 154, 255)
    } else {
        egui::Color32::from_rgb(37, 118, 214)
    }
}

/// 悬停时补足完整说明：图标按钮只有图形没有文字，
/// 不提示的话用户不知道点它会发生什么。
pub fn switch_hint(theme: ThemeKind) -> &'static str {
    match theme {
        ThemeKind::Light => "切换到暗色主题",
        ThemeKind::Dark => "切换到亮色主题",
    }
}

/// 任务状态的文字颜色，亮暗主题分别取值。
///
/// 旧实现把颜色硬编码在业务代码里，且亮色下黄 2.3:1、橙 2.9:1、绿 3.2:1，
/// 全都不达 WCAG 4.5:1，暗色下的蓝也只有约 3.4:1。这里统一按主题校准：
/// 亮色对白卡片底、暗色对卡片底 (36,40,48)，全部 ≥ 4.6:1。
pub fn status_color(dark_mode: bool, status: TaskStatus) -> egui::Color32 {
    if dark_mode {
        match status {
            TaskStatus::Waiting => egui::Color32::from_rgb(139, 148, 158),
            TaskStatus::Downloading => egui::Color32::from_rgb(108, 178, 255),
            TaskStatus::Canceling => egui::Color32::from_rgb(227, 179, 65),
            TaskStatus::Completed => egui::Color32::from_rgb(87, 217, 119),
            TaskStatus::Failed => egui::Color32::from_rgb(255, 123, 114),
            TaskStatus::Canceled => egui::Color32::from_rgb(240, 136, 62),
        }
    } else {
        match status {
            TaskStatus::Waiting => egui::Color32::from_rgb(108, 117, 125),
            TaskStatus::Downloading => egui::Color32::from_rgb(48, 122, 216),
            TaskStatus::Canceling => egui::Color32::from_rgb(154, 103, 0),
            TaskStatus::Completed => egui::Color32::from_rgb(21, 115, 71),
            TaskStatus::Failed => egui::Color32::from_rgb(214, 69, 65),
            TaskStatus::Canceled => egui::Color32::from_rgb(180, 83, 9),
        }
    }
}

/// 警告类提示文字的颜色（ffmpeg 未检测到、取消中这一类），两种主题下都达标。
pub fn warning_color(dark_mode: bool) -> egui::Color32 {
    status_color(dark_mode, TaskStatus::Canceling)
}

/// 成功类提示文字的颜色（ffmpeg 检测到、已完成这一类），两种主题下都达标。
pub fn success_color(dark_mode: bool) -> egui::Color32 {
    status_color(dark_mode, TaskStatus::Completed)
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
    // selection 恢复 egui 默认（浅色=淡蓝底、暗色=深蓝底）：egui 绘制文本选中时
    // 不改变文字颜色，只垫背景矩形。若把背景设成深蓝强调色，浅色主题下的深色文字
    // 落在深蓝底上就看不清选中了哪些字。强调色请用 accent_color()，见其注释。
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
    // active 态保留 egui 默认（浅色=深字、暗色=白字）：这里一旦改成「强调色底+白字」，
    // strong 文本会跟着变白——egui 的 strong_text_color() 直接取 active 的文字色，
    // 浅色主题下「运行日志」、选中 tab、表头、设置分组标题全部会看不清。
    // 需要强调色背景的按钮（主按钮/危险按钮）都显式设置了 fill 与文字色，不依赖这里。

    ctx.set_style(style);
}
