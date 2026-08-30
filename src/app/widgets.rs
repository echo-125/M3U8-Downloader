//! 界面通用控件与布局常量。
//!
//! 被 forms / task_list / dialogs 共用，不持有任何业务状态，
//! 只负责把 egui 即时模式的细节封装成稳定可复用的控件。

use eframe::egui::{self, Align, Color32, Layout, RichText};

use super::theme;

/// 表单左侧标签的固定宽度，保证各行输入框左缘对齐。
pub const FORM_LABEL_WIDTH: f32 = 76.0;
/// 表单控件高度，与按钮高度一致。
pub const FORM_CONTROL_HEIGHT: f32 = 30.0;
/// 表单行尾操作按钮的预留宽度，容纳「粘贴」「选择」这类两字按钮。
pub const FORM_BUTTON_WIDTH: f32 = 64.0;
/// 多行输入框每增加一行的高度增量。
pub const FORM_EXTRA_ROW_HEIGHT: f32 = 19.0;
/// 设置窗口标签宽度，容纳「默认单任务线程数」这类较长文案。
pub const SETTINGS_LABEL_WIDTH: f32 = 128.0;
/// 设置窗口内容区宽度固定，否则输入框宽度跟随窗口变化会与窗口尺寸互相推高，
/// 导致设置窗口每帧变大。
pub const SETTINGS_CONTENT_WIDTH: f32 = 660.0;
/// 设置窗口滚动区最大高度。
///
/// 必须用固定值而不是「可用高度减去按钮区」：后者会让滚动区高度依赖窗口高度，
/// 而窗口高度又由内容决定，形成正反馈导致窗口逐帧变大。
pub const SETTINGS_SCROLL_MAX_HEIGHT: f32 = 450.0;
/// 编辑任务窗口内容区宽度。
pub const EDIT_CONTENT_WIDTH: f32 = 520.0;
/// 日志行高，固定值让 ScrollArea 能按可视范围只渲染可见行。
pub const LOG_ROW_HEIGHT: f32 = 18.0;

/// 卡片容器：圆角面板配标题，给各功能区建立清晰的视觉边界。
pub fn card<R>(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        // 亮色下卡片是纯白、面板是浅灰，只靠 1px 淡描边几乎分不开边界；
        // 一条轻投影把卡片从面板上托起来。
        .shadow(egui::Shadow {
            offset: egui::vec2(0.0, 1.0),
            blur: 6.0,
            spread: 0.0,
            color: Color32::from_black_alpha(28),
        })
        .rounding(8.0)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).heading());
            ui.add_space(2.0);
            content(ui)
        })
        .inner
}

/// 强调色主按钮，用于界面上唯一的主要操作；次要操作用普通按钮即可形成层级。
/// 暗色主题的强调色偏亮，白字对比度不足，因此按主题切换文字颜色。
pub fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let text_color = if ui.visuals().dark_mode {
        Color32::from_rgb(12, 24, 40)
    } else {
        Color32::WHITE
    };
    let button = egui::Button::new(RichText::new(text).color(text_color))
        .fill(theme::accent_color(ui.visuals().dark_mode));
    ui.add_enabled(enabled, button)
}

/// 描边样式的次要按钮。与填充的主按钮并排时形成主次层级，
/// 避免一行里出现两个同样抢眼的实心按钮。
pub fn outline_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let accent = theme::accent_color(ui.visuals().dark_mode);
    let response = ui.add(
        egui::Button::new(RichText::new(text).color(accent))
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0_f32, accent)),
    );
    // 显式设置 fill 会盖掉 Button 内置的悬停反馈（它靠 weak_bg_fill 变化），
    // 这里补一道更粗的描边作为悬停提示，即时模式下下一帧生效。
    if response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            ui.visuals().widgets.hovered.rounding,
            egui::Stroke::new(2.0_f32, accent),
        );
    }
    response
}

/// 危险操作按钮：红色填充，用于退出确认等需要强调后果的操作。
pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(Color32::from_rgb(214, 69, 65)),
    )
}

/// 强调色描边按钮：比普通按钮醒目，又不像实心主按钮那样抢眼，
/// 用于「全部开始」这类次主操作，与工具栏里的主按钮形成层级。
pub fn accent_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let accent = theme::accent_color(ui.visuals().dark_mode);
    let button = egui::Button::new(RichText::new(text).color(accent))
        .fill(Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0_f32, accent));
    let response = ui.add_enabled(enabled, button);
    // 显式 fill 会盖掉 Button 内置的悬停反馈，补一道更粗的描边提示可点。
    if response.hovered() && response.enabled() {
        ui.painter().rect_stroke(
            response.rect,
            ui.visuals().widgets.hovered.rounding,
            egui::Stroke::new(2.0_f32, accent),
        );
    }
    response
}

/// 自绘图标按钮：按当前交互状态画按钮背景，然后执行自定义绘制函数。
///
/// 用于主题切换、日志折叠这类只有图形没有文字的按钮。Unicode 符号在
/// 中文环境里常被渲染成 emoji 或直接缺字，用 painter 画矢量最可靠。
pub fn icon_button(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    paint: impl FnOnce(&egui::Painter, egui::Rect, &egui::style::WidgetVisuals),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        ui.painter()
            .rect(rect, visuals.rounding, visuals.bg_fill, visuals.bg_stroke);
        paint(ui.painter(), rect, visuals);
    }
    response
}

/// 主题切换图标按钮。图标表示「点击后变成的样子」，与原先
/// 「暗色 / 亮色」文字按钮语义一致：亮色下显示月亮，暗色下显示太阳。
pub fn theme_switch_button(ui: &mut egui::Ui, theme: crate::config::ThemeKind) -> egui::Response {
    let response = icon_button(ui, egui::vec2(34.0, 30.0), |painter, rect, visuals| {
        let color = visuals.fg_stroke.color;
        match theme {
            crate::config::ThemeKind::Light => {
                draw_moon(painter, rect.center(), color, visuals.bg_fill)
            }
            crate::config::ThemeKind::Dark => draw_sun(painter, rect.center(), color),
        }
    });
    response.on_hover_text(super::theme::switch_hint(theme))
}

/// 折叠箭头按钮：展开时显示向下三角形，折叠时显示向右三角形。
pub fn chevron_button(ui: &mut egui::Ui, expanded: bool) -> egui::Response {
    icon_button(ui, egui::vec2(24.0, 22.0), |painter, rect, visuals| {
        let color = visuals.fg_stroke.color;
        let center = rect.center();
        let half = 4.0;
        let points = if expanded {
            // 向下：▾
            vec![
                center + egui::vec2(-half, -half * 0.6),
                center + egui::vec2(half, -half * 0.6),
                center + egui::vec2(0.0, half * 0.9),
            ]
        } else {
            // 向右：▸
            vec![
                center + egui::vec2(-half * 0.6, -half),
                center + egui::vec2(-half * 0.6, half),
                center + egui::vec2(half * 0.9, 0.0),
            ]
        };
        painter.add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    })
}

/// 太阳：中心圆 + 八条光芒线。
fn draw_sun(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let radius = 5.5;
    painter.circle_filled(center, radius, color);
    for index in 0..8 {
        let angle = index as f32 * std::f32::consts::TAU / 8.0;
        let (sin, cos) = angle.sin_cos();
        let direction = egui::vec2(cos, sin);
        painter.line_segment(
            [
                center + direction * (radius + 2.0),
                center + direction * (radius + 5.0),
            ],
            egui::Stroke::new(1.6_f32, color),
        );
    }
}

/// 月亮：主体圆靠左，用按钮背景色盖掉右上角形成月牙。
/// 覆盖色必须等于按钮填充色，所以由调用方传入，不能直接取面板色。
fn draw_moon(painter: &egui::Painter, center: egui::Pos2, color: Color32, background: Color32) {
    let radius = 7.0;
    let main = center + egui::vec2(-1.5, 0.0);
    painter.circle_filled(main, radius, color);
    let cut = main + egui::vec2(4.2, -2.0);
    painter.circle_filled(cut, radius - 2.2, background);
}

/// 统一的单行输入框：内边距与按钮保持一致，避免各处高低不齐。
pub fn input<'a>(text: &'a mut String) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).margin(egui::Margin::symmetric(8.0, 5.0))
}

/// 统一的多行输入框。
pub fn input_multiline<'a>(text: &'a mut String) -> egui::TextEdit<'a> {
    egui::TextEdit::multiline(text).margin(egui::Margin::symmetric(8.0, 5.0))
}

/// 统一样式的数字输入框：固定宽度与高度，与输入框和按钮保持一致。
pub fn number_input<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    suffix: &str,
) -> egui::Response {
    let mut widget = egui::DragValue::new(value).range(range).speed(1);
    if !suffix.is_empty() {
        widget = widget.suffix(suffix);
    }
    ui.add_sized([110.0, FORM_CONTROL_HEIGHT], widget)
}

/// 表单输入框宽度：占满当前行剩余空间；行尾有按钮时按按钮宽度预留。
///
/// 表单一律用 `horizontal` 而不是 `Grid`：Grid 在测量列宽时拿到的可用宽度极小，
/// 输入框的 `desired_width` 会被截断成几十像素，列宽随即被锁死。
pub fn form_field_width(ui: &egui::Ui, trailing_button: bool) -> f32 {
    let reserved = if trailing_button {
        FORM_BUTTON_WIDTH + ui.spacing().item_spacing.x
    } else {
        0.0
    };
    (ui.available_width() - reserved).max(160.0)
}

/// 错误提示用的红色，与状态列「已失败」保持一致。
pub const ERROR_COLOR: Color32 = Color32::from_rgb(214, 69, 65);

/// 表单行：固定宽度标签 + 撑满剩余空间的单行输入框，行尾可带一个按钮。
/// 返回行尾按钮是否被点击。
pub fn form_field(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    button: Option<&str>,
) -> bool {
    form_field_with_hint(ui, label, text, hint, button, None)
}

/// 与 `form_field` 相同，但 `error_hint` 非 `None` 时输入框描边变红，
/// 并在输入框正下方显示原因，让用户在打字时就能看到问题，而不是点了提交才弹 Toast。
pub fn form_field_with_hint(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    button: Option<&str>,
    error_hint: Option<String>,
) -> bool {
    let clicked = ui
        .horizontal(|ui| {
            right_label(ui, label, FORM_LABEL_WIDTH);
            let width = form_field_width(ui, button.is_some());
            let response = ui.add_sized([width, FORM_CONTROL_HEIGHT], input(text).hint_text(hint));
            if error_hint.is_some() {
                // TextEdit 没有描边 builder，错误态参照 outline_button 的做法
                // 在响应矩形上直接画。
                ui.painter().rect_stroke(
                    response.rect,
                    ui.visuals().widgets.inactive.rounding,
                    egui::Stroke::new(1.0_f32, ERROR_COLOR),
                );
            }
            match button {
                Some(label) => ui.button(label).clicked(),
                None => false,
            }
        })
        .inner;
    if let Some(hint) = error_hint {
        // 提示文字缩进到与输入框左缘对齐。
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 16.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(FORM_LABEL_WIDTH + ui.spacing().item_spacing.x);
                ui.label(RichText::new(hint).small().color(ERROR_COLOR));
            },
        );
    }
    clicked
}

/// 表单行：固定宽度标签 + 撑满剩余空间的多行输入框。
pub fn form_field_multiline(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    rows: usize,
) {
    ui.horizontal(|ui| {
        right_label(ui, label, FORM_LABEL_WIDTH);
        let width = form_field_width(ui, false);
        let height = FORM_CONTROL_HEIGHT + (rows.saturating_sub(1) as f32) * FORM_EXTRA_ROW_HEIGHT;
        ui.add_sized([width, height], input_multiline(text).hint_text(hint));
    });
}

/// 固定高度的竖直分隔线，用于水平排列的工具栏。
///
/// 不能直接调 `ui.separator()`：它在水平布局中把高度取成当前可用高度
/// （egui `separator.rs` 的 `available_space.y`），在滚动区内会把整行撑到几百像素高。
pub fn vertical_divider(ui: &mut egui::Ui, height: f32) {
    let width = ui.spacing().item_spacing.x;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter().vline(rect.center().x, rect.y_range(), stroke);
}

/// 固定宽度、右对齐的表单标签。
pub fn right_label(ui: &mut egui::Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 24.0),
        Layout::right_to_left(Align::Center),
        |ui| ui.label(text),
    );
}

/// 把文件对话框返回的路径转成字符串。
///
/// 不用 `to_string_lossy`：非 UTF-8 路径会被静默替换成 U+FFFD，一旦写进配置
/// 文件就无法恢复。这里显式拒绝，由调用方提示用户重新选择。
pub fn path_dialog_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(str::to_string)
}

/// 设置分组：分组标题 + 分割线。
pub fn section<R>(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(4.0);
    ui.label(RichText::new(title).strong());
    ui.separator();
    let result = content(ui);
    ui.add_space(6.0);
    result
}

/// 设置项行：右侧对齐的标签 + 控件。
pub fn settings_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        right_label(ui, label, SETTINGS_LABEL_WIDTH);
        content(ui)
    })
    .inner
}

pub fn format_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB", value / (1024.0 * 1024.0 * 1024.0))
    } else if value >= 1024.0 * 1024.0 {
        format!("{:.2} MB", value / (1024.0 * 1024.0))
    } else if value >= 1024.0 {
        format!("{:.2} KB", value / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes >= 60 {
        format!("{}小时{}分", minutes / 60, minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}分{seconds}秒")
    } else {
        format!("{seconds}秒")
    }
}
